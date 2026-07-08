package com.cruisemesh.app

import android.content.Context
import uniffi.cruisemesh_core.MessageStore

/** Lazily opens the single on-device [MessageStore] (messages + contacts). */
object AppStore {
    @Volatile
    private var instance: MessageStore? = null

    fun get(context: Context): MessageStore =
        instance ?: synchronized(this) {
            instance ?: MessageStore.open(
                context.applicationContext.filesDir.resolve("cruisemesh.sqlite").absolutePath,
            ).also { instance = it }
        }
}
