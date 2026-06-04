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
import android.os.ParcelFileDescriptor
import android.util.Log
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
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
                nativeOnSurfaceDestroyed()
            }
        })
        setContentView(surfaceView)
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
    }

    // Find the accessory (from the launch intent, or the live accessory list) and either
    // open it (if already permitted) or REQUEST permission — which pops the on-phone
    // "Allow RustScreen to access the USB accessory?" dialog. The implicit grant from the
    // USB_ACCESSORY_ATTACHED intent filter is per-install and resets on reinstall, so we
    // must be able to request it explicitly (otherwise openAccessory silently returns null).
    private fun maybeHandleAccessory() {
        if (sessionActive.get()) return // cheap early-out; openAndRun does the authoritative claim
        val accessory: UsbAccessory =
            intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
                ?: usb.accessoryList?.firstOrNull()
                ?: return
        if (usb.hasPermission(accessory)) {
            openAndRun(accessory)
        } else {
            Log.i(TAG, "no accessory permission yet — requesting (shows the Allow dialog)")
            val flags = if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
            val pi = PendingIntent.getBroadcast(
                this, 0, Intent(ACTION_USB_PERMISSION).setPackage(packageName), flags
            )
            usb.requestPermission(accessory, pi)
        }
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
        Log.i(TAG, "accessory opened — handing fd $fd to native echo loop (background thread)")
        // BL-02: once detachFd() returns, the raw fd is owned by nobody until nativeOnUsbFd
        // wraps it. If starting the thread throws, reclaim and close the fd (and release the
        // latch) so it isn't leaked.
        try {
            Thread({
                nativeOnUsbFd(fd) // blocks running echo_loop until the host closes (EOF) or errors
                // Session ended: release the latch so a subsequent attach can re-attempt.
                // (Prevents a permanent latch from sticking on a stale/half-registered
                // accessory — see the device-side handoff race in HARDWARE-FINDINGS.md.)
                Log.i(TAG, "echo session ended; releasing latch for re-attach")
                sessionActive.set(false)
            }, "usb-echo").start()
        } catch (t: Throwable) {
            runCatching { ParcelFileDescriptor.adoptFd(fd).close() }
            sessionActive.set(false)
            Log.e(TAG, "failed to start echo thread; reclaimed accessory fd", t)
        }
    }

    companion object {
        private const val TAG = "RustScreen"
        private const val ACTION_USB_PERMISSION = "com.rustscreen.client.USB_PERMISSION"

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
