package com.rustscreen.client

import android.app.Activity
import android.os.Bundle
import android.view.SurfaceView

class MainActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(SurfaceView(this))
        nativeInit()
    }

    companion object {
        init {
            System.loadLibrary("android_client")
        }

        @JvmStatic
        external fun nativeInit()
    }
}
