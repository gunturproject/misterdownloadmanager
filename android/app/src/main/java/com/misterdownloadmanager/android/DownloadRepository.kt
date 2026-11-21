package com.misterdownloadmanager.android

import android.app.Application
import android.content.Context
import android.net.Uri
import android.os.Environment
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

data class DownloadItem(
    val id: Long,
    val url: String,
    val name: String,
    val path: String,
    val downloaded: Long = 0,
    val total: Long = 0,
    val state: String = "Queued",
    val speed: Long = 0,
    val error: String? = null,
    val paused: Boolean = false,
    val finished: Boolean = false,
)

/** Owns the engine, the visible list, and the small JSON metadata file. */
class DownloadRepository private constructor(private val app: Application) : DownloadCallback {
    private val engine = DownloadEngine(app)
    private val prefs = app.getSharedPreferences("mdm", Context.MODE_PRIVATE)
    private val _items = MutableStateFlow<List<DownloadItem>>(emptyList())
    val items: StateFlow<List<DownloadItem>> = _items.asStateFlow()

    private var nextId = 1L
    private var lastPersist = 0L
    private val progressAt = HashMap<Long, Pair<Long, Long>>()

    init {
        engine.setCallback(this)
        restore()
    }

    fun add(url: String, connections: Int): Long {
        val dir = app.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: app.filesDir
        val target = uniqueFile(dir, nameFromUrl(url))
        val id = nextId++
        val item = DownloadItem(id = id, url = url, name = target.name, path = target.absolutePath)
        update { it + item }
        persist()

        engine.add(id, url, target.absolutePath, connections, 0L)
        DownloadService.start(app)
        return id
    }

    fun pause(id: Long) {
        engine.pause(id)
        update { list -> list.map { if (it.id == id) it.copy(paused = true, state = "Paused") else it } }
        persist()
    }

    fun resume(id: Long) {
        engine.resume(id)
        update { list -> list.map { if (it.id == id) it.copy(paused = false, state = "Resuming") else it } }
        persist()
    }

    fun remove(id: Long) {
        engine.remove(id)
        items.value.firstOrNull { it.id == id }?.let { item ->
            listOf(item.path, "$item.path.mdm.part", "$item.path.mdm.json").forEach { File(it).delete() }
        }
        progressAt.remove(id)
        update { list -> list.filterNot { it.id == id } }
        persist()
    }

    override fun onProgress(id: Long, downloaded: Long, total: Long, state: String) {
        val now = System.currentTimeMillis()
        val previous = progressAt[id]
        val speed = if (previous != null && now > previous.first && downloaded >= previous.second) {
            (downloaded - previous.second) * 1000L / (now - previous.first)
        } else 0L
        progressAt[id] = now to downloaded

        update { list ->
            list.map {
                if (it.id == id) {
                    it.copy(
                        downloaded = downloaded,
                        total = total,
                        speed = speed,
                        state = state,
                        error = null,
                        paused = state == "Paused" || it.paused,
                    )
                } else it
            }
        }
        persistThrottled()
    }

    override fun onFinished(id: Long, path: String, size: Long) {
        progressAt.remove(id)
        update { list ->
            list.map {
                if (it.id == id) {
                    it.copy(
                        path = path,
                        downloaded = size,
                        total = size,
                        state = "Done",
                        speed = 0,
                        error = null,
                        paused = false,
                        finished = true,
                    )
                } else it
            }
        }
        persist()
    }

    override fun onFailed(id: Long, message: String) {
        progressAt.remove(id)
        update { list ->
            list.map {
                if (it.id == id) it.copy(state = "Failed", error = message, speed = 0) else it
            }
        }
        persist()
    }

    override fun onPaused(id: Long) {
        progressAt.remove(id)
        update { list ->
            list.map {
                if (it.id == id) it.copy(state = "Paused", paused = true, speed = 0) else it
            }
        }
        persist()
    }

    private fun update(change: (List<DownloadItem>) -> List<DownloadItem>) {
        _items.value = change(_items.value)
    }

    private fun restore() {
        val restored = load()
        nextId = (restored.maxOfOrNull { it.id } ?: 0L) + 1
        update { restored }
        for (item in restored) {
            if (item.finished) continue
            engine.add(item.id, item.url, item.path, 8, item.downloaded)
            if (item.paused) engine.pause(item.id)
        }
    }

    private fun load(): List<DownloadItem> {
        val raw = prefs.getString(KEY_ITEMS, null) ?: return emptyList()
        return try {
            val array = JSONArray(raw)
            (0 until array.length()).map { i ->
                val o = array.getJSONObject(i)
                DownloadItem(
                    id = o.getLong("id"),
                    url = o.getString("url"),
                    name = o.getString("name"),
                    path = o.getString("path"),
                    downloaded = o.optLong("downloaded"),
                    total = o.optLong("total"),
                    state = o.optString("state", "Queued"),
                    paused = o.optBoolean("paused"),
                    finished = o.optBoolean("finished"),
                )
            }
        } catch (_: Exception) {
            emptyList()
        }
    }

    private fun persistThrottled() {
        val now = System.currentTimeMillis()
        if (now - lastPersist > 2_000L) persist()
    }

    private fun persist() {
        lastPersist = System.currentTimeMillis()
        val array = JSONArray()
        items.value.forEach { item ->
            array.put(
                JSONObject()
                    .put("id", item.id)
                    .put("url", item.url)
                    .put("name", item.name)
                    .put("path", item.path)
                    .put("downloaded", item.downloaded)
                    .put("total", item.total)
                    .put("state", item.state)
                    .put("paused", item.paused)
                    .put("finished", item.finished)
            )
        }
        prefs.edit().putString(KEY_ITEMS, array.toString()).apply()
    }

    private fun nameFromUrl(url: String): String {
        val segment = Uri.parse(url).lastPathSegment.orEmpty()
        val decoded = Uri.decode(segment).substringAfterLast('/')
        val clean = decoded.replace(Regex("[^A-Za-z0-9._-]"), "_").take(120)
        return clean.ifBlank { "download.bin" }
    }

    private fun uniqueFile(dir: File, name: String): File {
        var candidate = File(dir, name)
        var n = 1
        while (candidate.exists() || File("$candidate.mdm.json").exists()) {
            val dot = name.lastIndexOf('.')
            val base = if (dot > 0) name.substring(0, dot) else name
            val ext = if (dot > 0) name.substring(dot) else ""
            candidate = File(dir, "$base-$n$ext")
            n++
        }
        return candidate
    }

    companion object {
        private const val KEY_ITEMS = "items"

        @Volatile
        private var instance: DownloadRepository? = null

        fun get(context: Context): DownloadRepository =
            instance ?: synchronized(this) {
                instance ?: DownloadRepository(context.applicationContext as Application).also {
                    instance = it
                }
            }
    }
}
