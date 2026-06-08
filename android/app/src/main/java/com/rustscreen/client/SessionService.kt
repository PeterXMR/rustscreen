package com.rustscreen.client

import android.app.ActivityManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.Process
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Foreground service that owns the live USB decode session.
 *
 * Why this exists (on-device root cause, confirmed via logcat): the session used to run on a plain
 * `Thread` inside [MainActivity], so switching to another app made the process *cached* (oom-adj
 * 900+). Android then **froze** it (App Freezer → the decode thread halts → the USB read stalls →
 * the host sees a disconnect and reconnects) and the low-memory killer eventually **killed** it
 * (cold relaunch on return). That freeze/kill/reconnect churn — not a surface glitch — is what made
 * returning after a while show black-then-buggy for a few seconds.
 *
 * A foreground service keeps the process at foreground priority, so it is never frozen or killed:
 * the session stays connected across app switches and the existing surface-swap path
 * ([MediaCodecDecoder] `poll_surface` / `setOutputSurface`) handles the surface destroy/recreate
 * cleanly. The render surface itself stays owned by [MainActivity]'s `SurfaceView`; the process-global
 * native channels bridge it to the decode thread here.
 *
 * The JNI entry points stay declared on [MainActivity] (the Rust cdylib exports
 * `Java_com_rustscreen_client_MainActivity_nativeOnUsbFd`), so this service calls
 * `MainActivity.nativeOnUsbFd` rather than re-declaring native methods.
 */
class SessionService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            Log.i(TAG, "Stop requested from the notification — ending session")
            stopAndExit()
            return START_NOT_STICKY
        }
        // The accessory is opened by MainActivity (the working path — opening a parcelled UsbAccessory
        // here returned null on reconnect and spun the poll); we receive its already-detached fd. The
        // open accessory justifies the connectedDevice FGS type, so go foreground immediately.
        startInForeground()
        val fd = intent?.getIntExtra(EXTRA_FD, -1) ?: -1
        if (fd < 0) {
            Log.w(TAG, "no fd in start intent — stopping service")
            endNoSession()
            return START_NOT_STICKY
        }
        runSession(fd)
        // NOT sticky: a null-intent restart would carry no fd; the foreground Activity's poll
        // re-claims and restarts us on the next attach instead.
        return START_NOT_STICKY
    }

    private fun startInForeground() {
        val mgr = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= 26) {
            val ch = NotificationChannel(
                CHANNEL_ID,
                "RustScreen session",
                NotificationManager.IMPORTANCE_LOW,
            )
            ch.description = "Shown while RustScreen is streaming your Mac screen to this phone."
            ch.setShowBadge(false)
            mgr.createNotificationChannel(ch)
        }
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        val stopPi = PendingIntent.getService(
            this,
            0,
            Intent(this, SessionService::class.java).setAction(ACTION_STOP),
            flags,
        )
        val openPi = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            flags,
        )
        val notif: Notification = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("RustScreen")
            .setContentText("Streaming your Mac screen")
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentIntent(openPi)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(
                    null as android.graphics.drawable.Icon?,
                    "Stop",
                    stopPi,
                ).build(),
            )
            .build()
        try {
            if (Build.VERSION.SDK_INT >= 34) {
                startForeground(NOTIF_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
            } else {
                startForeground(NOTIF_ID, notif)
            }
            Log.i(TAG, "startForeground OK — process pinned at foreground priority")
        } catch (t: Throwable) {
            // If this fails the process stays cached → Android will freeze/kill it on background.
            // Surface it loudly so the failure mode is diagnosable rather than silent.
            Log.e(TAG, "startForeground FAILED — process will be frozen/killed when backgrounded", t)
        }
    }

    private fun runSession(fd: Int) {
        Log.i(TAG, "running decode session on fd $fd")
        Thread({
            val endedByHostStop = try {
                // Blocks running the live decode session until the host disconnects (EOF/error) or
                // sends Control::Bye. Returns true ONLY on a deliberate host stop (`rustscreen stop`).
                MainActivity.nativeOnUsbFd(fd)
            } catch (t: Throwable) {
                Log.e(TAG, "session thread threw", t)
                false
            } finally {
                // Always release the claim latch so a later attach can re-attempt.
                active.set(false)
            }
            Log.i(TAG, "session ended (endedByHostStop=$endedByHostStop)")
            if (endedByHostStop) {
                // Host sent Control::Bye (`rustscreen stop`): close the app entirely so the next
                // `rustscreen start` gets a clean COLD process (a warm process can fail to re-claim
                // the re-enumerated accessory).
                stopAndExit()
            } else {
                // Plain EOF/disconnect (e.g. replug): drop the foreground service; the foreground
                // Activity's accessory poll re-claims and restarts us on the next attach.
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        }, "usb-session").start()
    }

    /** Open failed: release the latch and tear the service down without a session. */
    private fun endNoSession() {
        active.set(false)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun stopAndExit() {
        active.set(false)
        // Remove the app's task(s) BEFORE killing the process. Without this, killing a process whose
        // activity was foreground (TOP) and whose task still exists makes Android restart the
        // activity — which is why `rustscreen stop` left the app relaunching/reconnecting instead of
        // staying closed. finishAndRemoveTask drops the task so there is nothing to restart (mirrors
        // the pre-service MainActivity.finishAndRemoveTask()).
        runCatching {
            getSystemService(ActivityManager::class.java)
                ?.appTasks
                ?.forEach { it.finishAndRemoveTask() }
        }
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        // Kill the process so the next launch is genuinely cold (fresh native state) — guarantees a
        // clean re-claim of the re-enumerated accessory on the next `rustscreen start`.
        Process.killProcess(Process.myPid())
    }

    companion object {
        private const val TAG = "RustScreen"
        private const val CHANNEL_ID = "rustscreen_session"
        private const val NOTIF_ID = 1
        const val EXTRA_FD = "fd"
        const val ACTION_STOP = "com.rustscreen.client.STOP_SESSION"

        /**
         * "A decode session is in progress" latch (was `MainActivity.sessionActive`). [MainActivity]
         * claims it with `compareAndSet(false, true)` before starting the service; this service
         * releases it when the session ends or an open attempt fails. Process-global so the claim is
         * atomic across the Activity's poll/permission paths and the service's session thread.
         */
        val active = AtomicBoolean(false)

        /** Start the foreground session service with an already-open accessory [fd] (detached by
         *  MainActivity; ownership moves to the service's session thread). */
        fun start(context: Context, fd: Int) {
            val i = Intent(context, SessionService::class.java)
                .putExtra(EXTRA_FD, fd)
            if (Build.VERSION.SDK_INT >= 26) {
                context.startForegroundService(i)
            } else {
                context.startService(i)
            }
        }
    }
}
