package com.rustscreen.client

import android.app.NativeActivity
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
import android.view.WindowManager

class MainActivity : NativeActivity() {

    private val usb by lazy { getSystemService(Context.USB_SERVICE) as UsbManager }

    // "A decode session is in progress" latch (BL-01) now lives on [SessionService.active] so the
    // claim is atomic across this Activity's poll/permission paths AND the service's session thread
    // (which releases it when the session ends). `maybeHandleAccessory`/`openAndRun` are reachable
    // from onCreate, onResume, onNewIntent AND the permission BroadcastReceiver, so a single
    // `compareAndSet` keeps "claim the accessory exactly once" atomic — two paths can't both claim it
    // / double-start the service. Not a permanent one-shot: it is released so a later attach (replug,
    // stale/half-registered accessory) can re-attempt.

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
    // idempotent (guarded by SessionService.active), so each tick is a cheap no-op once a session is live.
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
        // NativeActivity.onCreate loads libandroid_client (via the android.app.lib_name meta-data)
        // and calls its ANativeActivity_onCreate → the Rust `android_main` event loop, which OWNS
        // the render surface (decode-to-surface target, P4 Wave B, D3) and feeds the process-global
        // native channels the decode session consumes. This Activity adds ONLY the USB-accessory
        // glue on top — UsbManager has no NDK equivalent, so it stays in Kotlin. No SurfaceView /
        // setContentView: NativeActivity owns the window.
        super.onCreate(savedInstanceState)
        // Keep the phone awake for the whole session — a second screen that sleeps after the
        // display timeout is useless. Tied to this window, so it clears when the app leaves
        // the foreground. (PR #21 "Remaining" item, folded into the live-pipeline ladder item.)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        // Use the WHOLE panel, including the strip beside the Pixel 6a's punch-hole camera. In
        // landscape the default cutout policy (LAYOUT_IN_DISPLAY_CUTOUT_MODE_DEFAULT) forbids
        // content in the cutout region, so the OS reserves a ~1 cm black bar across that edge.
        // SHORT_EDGES renders into short-edge cutouts (the top-center punch-hole is one) in both
        // landscape orientations, so the decoded Mac frame fills the full surface — the camera
        // just floats over a thin sliver of content. Pure window policy: it only changes the
        // ANativeWindow size android_main hands MediaCodec; no per-frame hot-path cost, so it is
        // glass-to-glass-latency-neutral. Guarded for the API 28 (P) constant since minSdk is 26
        // (the Pixel 6a is API 33+, so this always applies on the real device).
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            window.attributes = window.attributes.apply {
                layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        }
        nativeInit()
        // Android 13+ gates posting notifications behind a runtime grant. The foreground service
        // raises process priority regardless, but request it so the "RustScreen — streaming" status
        // notification (with its Stop action) is actually shown. Fire-and-forget: the service runs
        // whether or not it is granted.
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) !=
            android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 0)
        }
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
        if (SessionService.active.get()) return // cheap early-out; openAndRun does the authoritative claim
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

    // Glue only: open the accessory HERE (the working path — opening a parcelled UsbAccessory inside
    // the service returned null on reconnect and spun the poll), detach its fd, and hand the fd to the
    // foreground [SessionService], which runs the blocking decode session on its own thread. Running
    // the session in a foreground service (not a plain Activity thread) is what stops Android from
    // freezing/killing the process when the user switches to another app — see [SessionService] for
    // the on-device root cause. The render surface stays owned by the Rust `android_main` event loop
    // and reaches the service's decode thread through the process-global native channels.
    private fun openAndRun(accessory: UsbAccessory) {
        // BL-01: claim the session atomically and exactly once, BEFORE any side effect. The service
        // releases the latch when the session ends.
        if (!SessionService.active.compareAndSet(false, true)) return
        val pfd: ParcelFileDescriptor = usb.openAccessory(accessory) ?: run {
            Log.w(TAG, "openAccessory returned null even though permission is granted")
            SessionService.active.set(false) // release so a later attach can retry
            return
        }
        val fd = pfd.detachFd()
        // Defense-in-depth: File::from_raw_fd(-1) on the Rust side is UB. detachFd() on a freshly
        // opened descriptor returns a valid fd, so this only guards a platform-specific deviation.
        if (fd < 0) {
            Log.e(TAG, "detachFd() returned invalid fd $fd; aborting")
            SessionService.active.set(false)
            return
        }
        Log.i(TAG, "accessory opened (fd $fd) — starting foreground session service")
        try {
            SessionService.start(this, fd)
        } catch (t: Throwable) {
            // startForegroundService is only disallowed from the background; the accessory poll runs
            // only while we are foreground, so this should not fire — but reclaim the fd + release the
            // latch if it does so a later attach can retry.
            runCatching { ParcelFileDescriptor.adoptFd(fd).close() }
            SessionService.active.set(false)
            Log.e(TAG, "failed to start session service; reclaimed accessory fd", t)
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
            // NativeActivity also loads this lib via the android.app.lib_name meta-data, but
            // System.loadLibrary is idempotent (the loader no-ops a second load of the same .so).
            // Keep it so the lib is guaranteed loaded whenever this companion is first touched —
            // including from SessionService's background thread calling nativeOnUsbFd below, which
            // can run when no fresh Activity load has happened.
            System.loadLibrary("android_client")
        }

        @JvmStatic
        external fun nativeInit()

        // Returns true iff the session ended because the host sent Control::Bye (`rustscreen stop`),
        // so the caller closes the app. EOF/disconnect (replug) and errors return false → stay alive.
        // Called from SessionService's session thread.
        @JvmStatic
        external fun nativeOnUsbFd(fd: Int): Boolean
    }
}
