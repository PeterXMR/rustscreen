package com.rustscreen.client

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.view.SurfaceView

class MainActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(SurfaceView(this))
        nativeInit()
        maybeHandleAccessory()
    }

    // HI-01: after the user taps "Allow" on the USB permission dialog, Android does NOT
    // re-run onCreate — it resumes the existing (singleTop) instance via onNewIntent and/or
    // onResume. We re-check the accessory in BOTH so the fd is never dropped.
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // Adopt the freshest intent so getParcelableExtra(EXTRA_ACCESSORY) reads from it.
        setIntent(intent)
        maybeHandleAccessory()
    }

    override fun onResume() {
        super.onResume()
        maybeHandleAccessory()
    }

    // Glue only (D0): receive the accessory attach, open it, detach the fd, hand it to Rust.
    // The echo loop itself lives in the Rust core behind the Transport seam — NOT here.
    // Idempotent-ish: openAccessory returns null until permission is granted (so repeated
    // calls before "Allow" are harmless no-ops); the single open→detachFd→nativeOnUsbFd path
    // is centralized here and invoked from onCreate, onNewIntent and onResume.
    private fun maybeHandleAccessory() {
        val usb = getSystemService(Context.USB_SERVICE) as UsbManager
        val accessory: UsbAccessory =
            intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY) ?: return
        // openAccessory returns null until the user grants the on-phone permission dialog.
        val pfd: ParcelFileDescriptor = usb.openAccessory(accessory) ?: return
        // detachFd() transfers sole ownership of the fd to native code (single-ownership).
        nativeOnUsbFd(pfd.detachFd())
    }

    companion object {
        init {
            System.loadLibrary("android_client")
        }

        @JvmStatic
        external fun nativeInit()

        @JvmStatic
        external fun nativeOnUsbFd(fd: Int)
    }
}
