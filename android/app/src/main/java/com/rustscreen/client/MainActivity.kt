package com.rustscreen.client

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.util.Log
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowManager
import java.util.concurrent.atomic.AtomicBoolean

class MainActivity : Activity() {

    private val usb by lazy { getSystemService(Context.USB_SERVICE) as UsbManager }

    // "An echo session is in progress" latch (BL-01). `maybeHandleAccessory`/`openAndRun`
    // are reachable from onCreate, onResume, onNewIntent AND the permission BroadcastReceiver,
    // so `AtomicBoolean.compareAndSet` makes "claim the accessory exactly once" a single
    // atomic step — two paths can't both open it / double-detach the fd / spawn two echo
    // threads. It is RELEASED when a session ends (echo loop returns) or any open attempt
    // fails, so a later attach can re-attempt. This deliberately is NOT a permanent one-shot:
    // a permanent latch could latch onto a stale/half-registered accessory (the device-side
    // handoff race in HARDWARE-FINDINGS.md) and then never retry.
    private val sessionActive = AtomicBoolean(false)

    // The current render surface, tracked across its create/destroy lifecycle. The native
    // SURFACE_SLOT handoff is consume-once (take_blocking removes it), and surfaceCreated only
    // fires once per surface lifetime — so a re-attach (a new decode session while the surface
    // already exists, same process) would find an empty slot and time out ("no render surface
    // within 10s"). Keeping the surface here lets openAndRun re-deposit it for each session.
    // Touched only on the UI thread today (surface callbacks + openAndRun); @Volatile is a
    // cheap guard in case a future caller ever reads it off-thread.
    @Volatile
    private var currentSurface: Surface? = null

    // Foreground accessory poll. The host (`rustscreen start`) brings this activity to the
    // foreground via `adb am start`, then performs the AOA switch. When the phone re-enumerates in
    // accessory mode, the ONLY built-in "accessory attached" signal is the USB_ACCESSORY_ATTACHED
    // intent — which Android Auto intercepts on some devices (it grabs the handshake, sits on it
    // ~10s, declines, and our app never receives the intent). So instead of depending on that
    // intent, while we are foreground we poll the accessory list a few times a second and claim the
    // accessory the instant it appears. This makes auto-start deterministic regardless of Android
    // Auto, with no fixed sleeps to tune. Polling runs ONLY while foreground (started in onResume,
    // stopped in onPause), so a backgrounded/closed app never claims an accessory — which is what
    // keeps Android Auto working normally when RustScreen isn't in use. maybeHandleAccessory() is
    // idempotent (guarded by sessionActive), so each tick is a cheap no-op once a session is live.
    private val accessoryPoller = Handler(Looper.getMainLooper())
    private val pollForAccessory = object : Runnable {
        override fun run() {
            maybeHandleAccessory()
            accessoryPoller.postDelayed(this, ACCESSORY_POLL_MS)
        }
    }

    // One-shot guard so the 250 ms accessory poll requests USB permission at most once per
    // accessory attach — without it the poll would re-fire requestPermission() (and re-pop the
    // "Allow?" dialog) ~4×/second while the user is still deciding. Reset when no accessory is
    // present (poll observes this every tick), so a fresh request is made when one (re)appears; we
    // deliberately do NOT reset on grant (a session starts) or deny (avoids an instant re-prompt
    // storm — replug or relaunch to retry). UI-thread only (poll, lifecycle, and the permission
    // receiver all run on the main thread), so a plain var is sufficient.
    private var permissionRequested = false

    // Receives the result of UsbManager.requestPermission() (the "Allow?" dialog).
    private val permissionReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action != ACTION_USB_PERMISSION) return
            val accessory: UsbAccessory? = intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
            val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
            Log.i(TAG, "USB permission result: granted=$granted accessory=$accessory")
            if (granted && accessory != null) openAndRun(accessory)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // The SurfaceView is the decode-to-surface target (P4 Wave B, D3). Its
        // SurfaceHolder.Callback hands the Surface to native code the moment it is created;
        // ANativeWindow_fromSurface in Rust turns it into the AMediaCodec render target.
        // Glue only — all decode logic lives in the Rust cdylib.
        val surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                Log.i(TAG, "surface created — handing to native decode-to-surface")
                currentSurface = holder.surface
                nativeOnSurface(holder.surface)
            }

            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                // No-op: the decoder reads dimensions from the H.264 SPS (csd-0); the surface
                // scales to fit. A resolution change is a Wave-2 (live reconfig) concern.
            }

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                // The Surface is going away; tell native code so it can stop rendering into a
                // dead window. The Rust side releases its ANativeWindow reference and ends the
                // decode loop.
                Log.i(TAG, "surface destroyed — notifying native")
                currentSurface = null
                nativeOnSurfaceDestroyed()
            }
        })
        setContentView(surfaceView)
        // Keep the phone awake for the whole session — a second screen that sleeps after the
        // display timeout is useless. Tied to this window, so it clears when the app leaves
        // the foreground. (PR #21 "Remaining" item, folded into the live-pipeline ladder item.)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        nativeInit()
        val filter = IntentFilter(ACTION_USB_PERMISSION)
        // Android 13+ requires an explicit export flag for runtime-registered receivers.
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(permissionReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            registerReceiver(permissionReceiver, filter)
        }
        maybeHandleAccessory()
    }

    override fun onDestroy() {
        super.onDestroy()
        accessoryPoller.removeCallbacks(pollForAccessory)
        runCatching { unregisterReceiver(permissionReceiver) }
    }

    // HI-01: after the user taps "Allow", Android resumes the existing (singleTop) instance
    // via onNewIntent/onResume rather than re-running onCreate — re-check in both.
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        maybeHandleAccessory()
    }

    override fun onResume() {
        super.onResume()
        maybeHandleAccessory()
        // Start the foreground accessory poll (see the field comment). removeCallbacks first so a
        // resume after a transient pause never stacks two poll loops.
        accessoryPoller.removeCallbacks(pollForAccessory)
        accessoryPoller.postDelayed(pollForAccessory, ACCESSORY_POLL_MS)
    }

    override fun onPause() {
        super.onPause()
        // Leaving the foreground: stop polling so a backgrounded app never claims the accessory.
        // This is what lets Android Auto behave exactly as before whenever RustScreen isn't in use.
        accessoryPoller.removeCallbacks(pollForAccessory)
    }

    // Find the accessory (from the launch intent, or the live accessory list) and either
    // open it (if already permitted) or REQUEST permission — which pops the on-phone
    // "Allow RustScreen to access the USB accessory?" dialog. The implicit grant from the
    // USB_ACCESSORY_ATTACHED intent filter is per-install and resets on reinstall, so we
    // must be able to request it explicitly (otherwise openAccessory silently returns null).
    private fun maybeHandleAccessory() {
        if (sessionActive.get()) return // cheap early-out; openAndRun does the authoritative claim
        val accessory: UsbAccessory? =
            intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY) ?: pickOurAccessory()
        if (accessory == null) {
            // No accessory present (idle, waiting for the host). Clear the one-shot request guard so
            // a fresh "Allow?" request is made when one (re)appears after the next AOA switch.
            permissionRequested = false
            return
        }
        if (usb.hasPermission(accessory)) {
            openAndRun(accessory)
        } else if (!permissionRequested) {
            permissionRequested = true
            Log.i(TAG, "no accessory permission yet — requesting (shows the Allow dialog)")
            val flags = if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
            val pi = PendingIntent.getBroadcast(
                this, 0, Intent(ACTION_USB_PERMISSION).setPackage(packageName), flags
            )
            usb.requestPermission(accessory, pi)
        }
    }

    // Prefer the accessory whose identity matches our AOA host, so the foreground poll never claims
    // an unrelated accessory (e.g. a car head unit). Falls back to the first accessory to preserve
    // the prior behavior on any device that reports a different/empty manufacturer string — i.e. this
    // can only ever be MORE selective than before, never less likely to find our host.
    private fun pickOurAccessory(): UsbAccessory? {
        val list = usb.accessoryList ?: return null
        return list.firstOrNull { it.manufacturer == EXPECTED_MANUFACTURER } ?: list.firstOrNull()
    }

    // Glue only (D0): open the accessory, detach the fd, hand it to Rust. The blocking echo
    // loop MUST run off the UI thread (else the app ANRs and the echo dies), so spawn a
    // dedicated thread; the fd's sole ownership moves into native code there.
    private fun openAndRun(accessory: UsbAccessory) {
        // BL-01: claim the session atomically and exactly once, BEFORE any side effect.
        if (!sessionActive.compareAndSet(false, true)) return
        val pfd: ParcelFileDescriptor = usb.openAccessory(accessory) ?: run {
            Log.w(TAG, "openAccessory returned null even though permission is granted")
            sessionActive.set(false) // release so a later attach can retry
            return
        }
        val fd = pfd.detachFd()
        // Defense-in-depth: never hand a negative fd to native code — File::from_raw_fd(-1)
        // is undefined behavior. detachFd() on this freshly-opened descriptor returns a valid
        // fd (it throws IllegalStateException only if already closed, which can't happen right
        // after openAccessory), so this guard is belt-and-suspenders against a platform-
        // specific deviation; on a hit, release the latch so a later attach can retry. (BL-04)
        if (fd < 0) {
            Log.e(TAG, "detachFd() returned invalid fd $fd; aborting")
            sessionActive.set(false)
            return
        }
        Log.i(TAG, "accessory opened — handing fd $fd to native decode session (background thread)")
        // Re-deposit the surface for THIS session. The native handoff is consume-once, so without
        // this a re-attach (surface already created, slot drained by a previous session) would
        // time out waiting for a surface that surfaceCreated will never re-announce. If the surface
        // isn't up yet (cold launch-by-plug), this is null and surfaceCreated deposits it later.
        currentSurface?.let { surface ->
            Log.i(TAG, "re-depositing existing surface for new decode session")
            nativeOnSurface(surface)
        }
        // BL-02: once detachFd() returns, the raw fd is owned by nobody until nativeOnUsbFd
        // wraps it. If starting the thread throws, reclaim and close the fd (and release the
        // latch) so it isn't leaked.
        try {
            Thread({
                try {
                    // Blocks running the live decode session (rendezvous with the surface, then
                    // run_session) until the host disconnects (EOF) or errors.
                    nativeOnUsbFd(fd)
                } finally {
                    // ALWAYS release the latch — even if the thread body throws or is interrupted —
                    // so a subsequent attach can re-attempt. A missed release latches sessionActive
                    // forever and blocks every future reconnect. (The Rust side wraps its body in
                    // catch_unwind so nativeOnUsbFd shouldn't throw, but the finally keeps the
                    // invariant regardless of future changes — see the device-side handoff race in
                    // HARDWARE-FINDINGS.md.)
                    Log.i(TAG, "session ended; releasing latch for re-attach")
                    sessionActive.set(false)
                }
            }, "usb-session").start()
        } catch (t: Throwable) {
            runCatching { ParcelFileDescriptor.adoptFd(fd).close() }
            sessionActive.set(false)
            Log.e(TAG, "failed to start session thread; reclaimed accessory fd", t)
        }
    }

    companion object {
        private const val TAG = "RustScreen"
        private const val ACTION_USB_PERMISSION = "com.rustscreen.client.USB_PERMISSION"

        // Foreground accessory-poll cadence. ~4 Hz: claims the accessory within ~250 ms of the AOA
        // switch (a one-time connect cost, not on the glass-to-glass hot path) while costing only a
        // cheap atomic load per tick once a session is live.
        private const val ACCESSORY_POLL_MS = 250L

        // Must equal AOA control-request-52 identity string 0 (manufacturer) in
        // crates/macos-host/src/aoa.rs and res/xml/accessory_filter.xml.
        private const val EXPECTED_MANUFACTURER = "RustScreen"

        init {
            System.loadLibrary("android_client")
        }

        @JvmStatic
        external fun nativeInit()

        @JvmStatic
        external fun nativeOnUsbFd(fd: Int)

        // P4 Wave B (DEC-01): hand the SurfaceView's Surface to the native AMediaCodec
        // decode-to-surface adapter (ANativeWindow_fromSurface), and signal teardown when
        // the surface is destroyed.
        @JvmStatic
        external fun nativeOnSurface(surface: Surface)

        @JvmStatic
        external fun nativeOnSurfaceDestroyed()
    }
}
