package dev.moq

import android.content.Context

object PlatformTLS {
    init {
        System.loadLibrary("moq_ffi")
    }

    @JvmStatic
    fun initialize(context: Context) {
        initializeNative(context.applicationContext)
    }

    @JvmStatic
    private external fun initializeNative(context: Context)
}
