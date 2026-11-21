package com.misterdownloadmanager.android

import android.content.Context

/** Progress and terminal events sent from the Rust engine's worker threads. */
interface DownloadCallback {
    fun onProgress(id: Long, downloaded: Long, total: Long, state: String)
    fun onFinished(id: Long, path: String, size: Long)
    fun onFailed(id: Long, message: String)
    fun onPaused(id: Long)
}

/**
 * Thin Kotlin wrapper around the native library. All native calls are
 * synchronized because the Rust side keeps one engine handle and one callback.
 */
class DownloadEngine(context: Context) {
    private var handle: Long = 0L

    init {
        System.loadLibrary("mdm_android")
        val dataDir = context.applicationContext.getExternalFilesDir(null)?.absolutePath ?: ""
        handle = nativeInit(dataDir, context.applicationContext)
        if (handle == 0L) error("MDM engine failed to initialize")
    }

    private fun requireHandle(): Long {
        check(handle != 0L) { "MDM engine has been destroyed" }
        return handle
    }

    @Synchronized
    fun setCallback(callback: DownloadCallback) = nativeSetCallback(requireHandle(), callback)

    /** @return the same id, so the caller can persist it before the first event arrives. */
    @Synchronized
    fun add(id: Long, url: String, dest: String, connections: Int, shownBytes: Long): Long =
        nativeAdd(requireHandle(), id, url, dest, connections, shownBytes)

    @Synchronized
    fun pause(id: Long) = nativePause(requireHandle(), id)

    @Synchronized
    fun resume(id: Long) = nativeResume(requireHandle(), id)

    @Synchronized
    fun remove(id: Long) = nativeRemove(requireHandle(), id)

    @Synchronized
    fun snapshot(id: Long): String = nativeSnapshot(requireHandle(), id)

    @Synchronized
    fun list(): String = nativeList(requireHandle())

    @Synchronized
    fun destroy() {
        if (handle != 0L) {
            nativeDestroy(handle)
            handle = 0L
        }
    }

    private external fun nativeInit(dataDir: String, context: Context): Long
    private external fun nativeSetCallback(handle: Long, callback: DownloadCallback)
    private external fun nativeAdd(
        handle: Long,
        id: Long,
        url: String,
        dest: String,
        connections: Int,
        shownBytes: Long,
    ): Long
    private external fun nativePause(handle: Long, id: Long)
    private external fun nativeResume(handle: Long, id: Long)
    private external fun nativeRemove(handle: Long, id: Long)
    private external fun nativeSnapshot(handle: Long, id: Long): String
    private external fun nativeList(handle: Long): String
    private external fun nativeDestroy(handle: Long)
}
