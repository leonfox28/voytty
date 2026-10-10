package io.github.leonfox28.zterm

import androidx.test.platform.app.InstrumentationRegistry
import io.github.leonfox28.zterm.nativebridge.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import java.security.KeyStore
import java.util.UUID

/** Real repository and storage; only the native boundary controls response timing. */
class SessionRefreshTest {
    @Test fun cancelInitialConnectionStopsThePendingNativeCall() = runBlocking {
        fixture { repository, runtime ->
            val requested = CompletableDeferred<Unit>()
            val cancelled = CompletableDeferred<Unit>()
            runtime.list = {
                requested.complete(Unit)
                try { awaitCancellation() } finally { cancelled.complete(Unit) }
            }
            repository.connectHost(HOST)
            withTimeout(5_000) { requested.await() }
            assertTrue(repository.state.value.connecting)
            repository.goHome()
            withTimeout(1_000) {
                cancelled.await()
                repository.state.first { it.route == Route.Home && it.hostId == null && !it.busy }
            }
            assertNull(repository.state.value.error)
            assertTrue(runtime.attachments.isEmpty())
            assertEquals(0, runtime.defaultConnections)

            runtime.list = { emptyList() }
            repository.connectHost(HOST)
            withTimeout(5_000) { repository.state.first { it.route == Route.Terminal && !it.busy } }
            assertEquals("a new user connection is not blocked by the cancelled call", 1, runtime.defaultConnections)
        }
    }

    @Test fun failedListAfterExactRestoreDoesNotFailTheActiveConnection() = runBlocking {
        fixture { repository, runtime ->
            runtime.list = { throw NativeException.RequestFailed("deadline_exceeded") }
            repository.activityStarted()
            withTimeout(5_000) { repository.state.first { it.route == Route.Terminal && !it.busy } }
            assertEquals("active", repository.frame.value?.state)
            assertEquals(SESSION, repository.state.value.sessionId)
            assertEquals(listOf(SESSION), runtime.attachments)
            assertNull("list failure must not become a terminal connection error", repository.state.value.error)
            assertEquals("deadline_exceeded", repository.state.value.sessionListError)
            assertTrue(repository.text("once"))
            withTimeout(5_000) { runtime.input.await() }
            assertEquals(listOf("once"), runtime.inputs)
            runtime.list = { emptyList() }
            repository.refreshSessionList()
            withTimeout(5_000) { repository.state.first { it.sessionListError == null && !it.sessionsRefreshing } }
            assertEquals("retry reads metadata without replacing the terminal", listOf(SESSION), runtime.attachments)
            assertEquals(0, runtime.defaultConnections)
        }
    }

    @Test fun retiredListFailureCannotSetAnErrorOnHome() = runBlocking {
        fixture { repository, runtime ->
            repository.activityStarted()
            withTimeout(5_000) { repository.state.first { it.route == Route.Terminal && !it.busy } }
            val requested = CompletableDeferred<Unit>()
            val release = CompletableDeferred<Unit>()
            runtime.list = {
                requested.complete(Unit)
                release.await()
                throw NativeException.RequestFailed("deadline_exceeded")
            }
            while (runtime.queryFinished.tryReceive().isSuccess) { /* Drain restoration's query. */ }
            repository.togglePanel()
            withTimeout(5_000) { requested.await() }
            repository.goHome()
            withTimeout(5_000) { repository.state.first { it.route == Route.Home && it.hostId == null } }
            release.complete(Unit)
            withTimeout(5_000) { runtime.queryFinished.receive() }
            yield()
            assertNull("a retired host query cannot contaminate Home", repository.state.value.error)
            assertNull(repository.state.value.sessionListError)
            assertNull(repository.frame.value)
        }
    }

    @Test fun newerListSuccessRetiresEarlierFailure() = runBlocking {
        fixture { repository, runtime ->
            repository.activityStarted()
            withTimeout(5_000) { repository.state.first { it.route == Route.Terminal && !it.busy } }
            while (runtime.queryFinished.tryReceive().isSuccess) { /* Drain restoration. */ }
            val requested = CompletableDeferred<Unit>()
            val release = CompletableDeferred<Unit>()
            runtime.list = {
                requested.complete(Unit); release.await()
                throw NativeException.RequestFailed("deadline_exceeded")
            }
            repository.togglePanel()
            withTimeout(5_000) { requested.await() }
            runtime.list = { emptyList() }
            repository.refreshSessionList()
            withTimeout(5_000) { runtime.queryFinished.receive() }
            assertFalse(repository.state.value.sessionsRefreshing)
            release.complete(Unit)
            withTimeout(5_000) { runtime.queryFinished.receive() }
            yield()
            assertNull(repository.state.value.sessionListError)
            assertNull(repository.state.value.error)
            assertEquals("active", repository.frame.value?.state)
        }
    }

    @Test fun failedInitialListNeverCreatesASession() = runBlocking {
        fixture { repository, runtime ->
            runtime.list = { throw NativeException.RequestFailed("deadline_exceeded") }
            repository.connectHost(HOST)
            withTimeout(5_000) { repository.state.first { it.route == Route.Terminal && !it.busy } }
            assertEquals("deadline_exceeded", repository.state.value.error)
            assertNull(repository.frame.value)
            assertTrue(runtime.attachments.isEmpty())
            assertEquals(0, runtime.defaultConnections)
        }
    }

    @Test fun retiredInitialListCannotSetAnErrorOnHome() = runBlocking {
        fixture { repository, runtime ->
            val requested = CompletableDeferred<Unit>()
            val release = CompletableDeferred<Unit>()
            runtime.list = {
                requested.complete(Unit); release.await()
                throw NativeException.RequestFailed("deadline_exceeded")
            }
            repository.connectHost(HOST)
            withTimeout(5_000) { requested.await() }
            repository.goHome()
            withTimeout(5_000) { repository.state.first { it.route == Route.Home && it.hostId == null } }
            release.complete(Unit)
            withTimeout(5_000) { repository.state.first { !it.busy } }
            assertNull("the mandatory opening query also loses authority after Home", repository.state.value.error)
            assertNull(repository.state.value.sessionListError)
            assertTrue(runtime.attachments.isEmpty())
            assertEquals(0, runtime.defaultConnections)
        }
    }

    @Test fun cancelledMetadataQueryDoesNotBecomeAConnectionError() = runBlocking {
        fixture { repository, runtime ->
            runtime.list = { throw CancellationException("fixture cancellation") }
            repository.activityStarted()
            withTimeout(5_000) { repository.state.first { it.route == Route.Terminal && !it.busy } }
            assertNull(repository.state.value.error)
            assertNull(repository.state.value.sessionListError)
            assertFalse(repository.state.value.sessionsRefreshing)
            assertEquals("active", repository.frame.value?.state)
        }
    }

    @Test fun missingExactSessionStillReportsFailureWithoutCreatingAReplacement() = runBlocking {
        fixture { repository, runtime ->
            runtime.attachFailure = "session_not_found"
            repository.activityStarted()
            withTimeout(5_000) { repository.state.first { it.route == Route.Terminal && !it.busy } }
            assertEquals("session_not_found", repository.state.value.error)
            assertNull(repository.frame.value)
            assertEquals(listOf(SESSION), runtime.attachments)
            assertEquals(0, runtime.defaultConnections)
        }
    }

    private suspend fun fixture(test: suspend (AppRepository, Runtime) -> Unit) = withContext(Dispatchers.Main.immediate) {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val token = UUID.randomUUID().toString()
        val directory = File(context.cacheDir, "session-refresh-$token")
        val alias = "zterm.test.session-refresh.$token"
        val store = AppStore(context, directory, alias)
        withContext(Dispatchers.IO) {
            store.loadOrCreateSeed().fill(0)
            store.save(SavedState(hosts = listOf(SavedHost(HOST, "Fixture", emptyList(), SESSION)),
                activeTerminal = RecentConnection(HOST, SESSION)))
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val runtime = Runtime()
        val repository = AppRepository(context, runtime, scope = scope, store = store)
        try {
            withTimeout(5_000) { repository.state.first { it.initialized } }
            test(repository, runtime)
        } finally {
            repository.goHome()
            withTimeout(5_000) { repository.state.first { it.route == Route.Home && it.hostId == null } }
            scope.coroutineContext.job.cancelAndJoin()
            runtime.close()
            withContext(Dispatchers.IO) {
                directory.deleteRecursively()
                KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(alias) }
            }
        }
    }

    private class Runtime : NativeRuntime(NoHandle) {
        var list: suspend () -> List<NativeSession> = { emptyList() }
        var attachFailure: String? = null
        val attachments = mutableListOf<String>()
        var defaultConnections = 0
        val inputs = mutableListOf<String>()
        val input = CompletableDeferred<Unit>()
        val queryFinished = kotlinx.coroutines.channels.Channel<Unit>(kotlinx.coroutines.channels.Channel.UNLIMITED)
        override suspend fun initialize(seed: ByteArray, dnsServers: List<NativeDnsServer>, hosts: List<NativeHost>) = "fixture"
        override suspend fun updateDns(servers: List<NativeDnsServer>) = Unit
        override suspend fun listSessions(host: String): List<NativeSession> {
            try { return list() } finally { queryFinished.trySend(Unit) }
        }
        override suspend fun connectDefaultTerminal(host: String, viewport: NativeViewport, dark: Boolean): NativeTerminal {
            defaultConnections++
            error("exact restoration must not create a default Session")
        }
        override suspend fun connectTerminal(host: String, session: String, viewport: NativeViewport, dark: Boolean, takeover: Boolean): NativeTerminal {
            attachments.add(session)
            attachFailure?.let { throw NativeException.RequestFailed(it) }
            return object : NativeTerminal(NoHandle) {
                override fun sessionId() = session
                override fun currentFrame() = activeFrame()
                override suspend fun waitForFrame(afterGeneration: ULong): NativeFrame = awaitCancellation()
                override suspend fun nextNotification(): NativeNotification = awaitCancellation()
                override suspend fun detach() = Unit
                override suspend fun commitText(inputEpoch: ULong, text: String, modifiers: UByte, paste: Boolean) {
                    inputs.add(text); input.complete(Unit)
                }
            }
        }
    }

    companion object {
        private const val HOST = "1111111111111111111111111111111111111111111111111111111111111111"
        private const val SESSION = "22222222222222222222222222222222"
        private fun activeFrame() = NativeFrame(
            applicationTitle = "", cursorShape = NativeCursorShape.BLOCK, cursorBlinking = false,
            connectionPath = NativeConnectionPath.RELAY, rttMs = 10u,
            pointerMode = NativePointerMode.NONE, activeScreen = NativeActiveScreen.MAIN, source = null,
            stats = NativeNavigationStats(0u, 0u, 0u, 0u, 0u, 0u, 0u), notice = null,
            inputEpoch = 1u, geometryGeneration = 1u, inputReady = true,
            historyOffset = 0u, windowOffset = 0u, contentGeneration = 1u,
            firstRow = 0, selection = null, generation = 1u, state = "active", error = null,
            viewport = NativeViewport(24u, 80u), rows = emptyList(), cursorRow = 0u, cursorColumn = 0u,
            cursorVisible = false, cursorColor = 0xffffffffu, background = 0xff000000u, historyMaximum = 0u,
        )
    }
}
