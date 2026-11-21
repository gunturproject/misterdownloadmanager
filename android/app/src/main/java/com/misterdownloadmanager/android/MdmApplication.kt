package com.misterdownloadmanager.android

import android.app.Application

class MdmApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        // Loads the native engine and restores unfinished downloads.
        DownloadRepository.get(this)
    }
}
