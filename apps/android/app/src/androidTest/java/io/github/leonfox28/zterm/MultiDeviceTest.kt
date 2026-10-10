package io.github.leonfox28.zterm

import android.os.SystemClock
import android.content.ContentValues
import android.net.Uri
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.provider.MediaStore
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.File

/** Private, opt-in command mailbox for the disposable three-device coordinator.
 * All terminal actions use the Application's real repository and Activity.
 * No transport warming, injected connection paths, or mutation retries.
 */
class MultiDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val app = ApplicationProvider.getApplicationContext<ZtermApplication>()
    private val repository get() = app.repository
    private var uploadUri: Uri? = null
    private var deviceId: String? = null
    private fun main(action: () -> Unit) = instrumentation.runOnMainSync(action)
    private suspend fun idle() = withTimeout(60_000) {
        while (!repository.state.value.initialized || repository.state.value.busy) delay(20)
    }

    private fun snapshot(): JSONObject {
        val result = JSONObject()
        main {
            val state = repository.state.value
            val frame = repository.frame.value
            val upload = repository.uploads.state.value
            val connectivity = app.getSystemService(ConnectivityManager::class.java)
            val network = connectivity.activeNetwork
            val capabilities = connectivity.getNetworkCapabilities(network)
            result.put("networkId", network?.networkHandle?.toString())
                .put("network", when {
                    capabilities?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true -> "wifi"
                    capabilities?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true -> "cellular"
                    else -> "other"
                })
            result.put("deviceId", deviceId).put("host", state.hostId).put("session", state.sessionId)
                .put("state", frame?.state ?: "none").put("error", state.error ?: frame?.error)
                .put("sessionListError", state.sessionListError).put("sessionsRefreshing", state.sessionsRefreshing)
                .put("busy", state.busy).put("inputReady", frame?.inputReady ?: false)
                .put("inputEpoch", frame?.inputEpoch?.toString())
                .put("path", frame?.connectionPath?.name?.lowercase() ?: "unknown")
                .put("rows", frame?.viewport?.rows?.toInt())
                .put("columns", frame?.viewport?.columns?.toInt())
                .put("text", frame?.rows?.joinToString("\n") { row -> row.cells.joinToString("") { it.text } } ?: "")
                .put("route", state.route.name).put("restoring", state.restoringSession)
                .put("background", state.saved.preferences.keepBackgroundConnection)
                .put("notificationVisible", repository.backgroundConnection.visibleNotification())
                .put("upload", upload?.let { JSONObject().put("phase", it.phase).put("bytes", it.acceptedBytes).put("error", it.error) })
                .put("hosts", JSONArray(state.saved.hosts.map { JSONObject().put("id", it.id).put("name", it.name) }))
                .put("elapsedMs", SystemClock.elapsedRealtime())
        }
        return result
    }

    @Test fun coordinate() = runBlocking {
        val token = InstrumentationRegistry.getArguments().getString("multiDevice") ?: ""
        assumeTrue("explicit isolated three-device fixture", token.matches(Regex("[a-f0-9]{32}")))
        val directory = File(app.cacheDir, "multi-device-$token").apply { mkdirs() }
        val command = File(directory, "command.json")
        val response = File(directory, "response.json")
        val status = File(directory, "status.json")
        fun publish(file: File, value: JSONObject) {
            val temporary = File(directory, file.name + ".tmp")
            temporary.writeText(value.toString())
            check(temporary.renameTo(file))
        }
        ActivityScenario.launch(MainActivity::class.java).use { activity ->
            idle()
            // Initialization has already bound the Application's sole endpoint.
            // This idempotent local call validates its identity and returns only
            // the public ID; it sends no peer request and exports no seed.
            check(repository.state.value.initialized)
            val seed = AppStore(app).loadOrCreateSeed()
            try { deviceId = app.runtime.initialize(seed, emptyList(), emptyList()) }
            finally { seed.fill(0) }
            app.diagnostics.setEnabled(true)
            File(directory, "ready").writeText("ready")
            // The outer runner owns time bounds and process termination. The
            // absolute guard prevents an abandoned test from running forever.
            val deadline = SystemClock.elapsedRealtime() + 4 * 60 * 60 * 1000
            while (SystemClock.elapsedRealtime() < deadline) {
                publish(status, snapshot())
                if (!command.exists()) { delay(100); continue }
                val request = JSONObject(command.readText())
                check(command.delete())
                val result = JSONObject().put("id", request.getInt("id"))
                var stop = false
                try {
                    when (request.getString("op")) {
                        "snapshot" -> Unit
                        "pair" -> {
                            val ticket = File(app.filesDir, "multi-device-ticket.txt")
                            try { main { repository.pair(ticket.readText().trim()) }; idle() }
                            finally { ticket.delete() }
                        }
                        "connect" -> {
                            val host = repository.state.value.saved.hosts.single { it.name == request.getString("host") }
                            check(host.name.startsWith("multi-device-"))
                            main { repository.connectHost(host.id) }
                            idle()
                        }
                        "select" -> {
                            main { repository.selectSession(request.getString("session"), request.optBoolean("takeover")) }
                            idle()
                        }
                        "input" -> main { result.put("accepted", repository.text(request.getString("text"))) }
                        "resize" -> main { repository.measure(request.getInt("rows"), request.getInt("columns")) }
                        "home" -> {
                            main { repository.goHome() }
                            withTimeout(10_000) { while (repository.state.value.hostId != null) delay(20) }
                        }
                        "retry" -> { main { repository.retry() }; idle() }
                        "background" -> activity.moveToState(Lifecycle.State.CREATED)
                        "foreground" -> activity.moveToState(Lifecycle.State.RESUMED)
                        "recreate" -> { activity.recreate(); idle() }
                        "keepBackground" -> main {
                            repository.updatePreferences { it.copy(keepBackgroundConnection = request.getBoolean("enabled")) }
                        }
                        "uploadPrepare" -> {
                            // Retain the real application-owned upload controller,
                            // without opening a system picker in the coordinator.
                            activity.moveToState(Lifecycle.State.CREATED)
                            val uri = requireNotNull(app.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                                ContentValues().apply {
                                    put(MediaStore.MediaColumns.DISPLAY_NAME, "multi-device-$token.bin")
                                    put(MediaStore.MediaColumns.MIME_TYPE, "application/octet-stream")
                                    put(MediaStore.MediaColumns.RELATIVE_PATH, "Download")
                                }))
                            uploadUri = uri
                            requireNotNull(app.contentResolver.openOutputStream(uri)).use { output ->
                                val block = ByteArray(64_000) { (it % 251).toByte() }
                                repeat(750) { output.write(block) }
                            }
                            main { repository.uploads.choose(UploadPicker.File) }
                            withTimeout(30_000) {
                                while (repository.uploads.state.value?.phase != "picking") {
                                    check(repository.uploads.state.value?.phase != "failed")
                                    delay(20)
                                }
                            }
                        }
                        "uploadStart" -> main {
                            val id = requireNotNull(repository.uploads.state.value).id
                            check(repository.uploads.pickerLaunched(id))
                            repository.uploads.selected(id, requireNotNull(uploadUri))
                        }
                        "uploadCleanup" -> {
                            main { repository.uploads.cancel() }
                            uploadUri?.let { app.contentResolver.delete(it, null, null) }
                            uploadUri = null
                            activity.moveToState(Lifecycle.State.RESUMED)
                        }
                        "lock", "unlock" -> {
                            val commands = if (request.getString("op") == "lock") listOf("input keyevent KEYCODE_SLEEP")
                                else listOf("input keyevent KEYCODE_WAKEUP", "wm dismiss-keyguard")
                            commands.forEach { shell ->
                                instrumentation.uiAutomation.executeShellCommand(shell).use {
                                    android.os.ParcelFileDescriptor.AutoCloseInputStream(it).readBytes()
                                }
                            }
                        }
                        "stop" -> stop = true
                        else -> error("unsupported coordinator operation")
                    }
                    result.put("ok", true).put("snapshot", snapshot())
                } catch (error: Exception) {
                    result.put("ok", false).put("exception", error.javaClass.simpleName)
                        .put("message", error.message).put("snapshot", snapshot())
                }
                // A completed command and its immediately following observation
                // must share the same state, including cleared errors.
                publish(status, result.getJSONObject("snapshot"))
                publish(response, result)
                if (stop) return@runBlocking
            }
            error("coordinator abandoned its four-hour mailbox")
        }
    }
}
