package io.github.leonfox28.zterm

import android.text.InputType
import android.text.Selection
import android.view.KeyEvent
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import androidx.test.platform.app.InstrumentationRegistry
import io.github.leonfox28.zterm.nativebridge.*
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.first
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import java.security.KeyStore
import java.util.UUID

/** The production editor contract without an IME brand, transport or live host. */
class TerminalInputConnectionTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()

    @Test fun terminalEnterUsesKeyEventsWhileCommittedNewlinesRemainText() = runBlocking {
        inputFixture { view, inputs ->
            val info = EditorInfo().apply { imeOptions = EditorInfo.IME_FLAG_NO_ENTER_ACTION }
            val connection = view.onCreateInputConnection(info) as BaseInputConnection
            assertEquals(EditorInfo.IME_ACTION_NONE, info.imeOptions and EditorInfo.IME_MASK_ACTION)
            assertEquals(0, info.imeOptions and EditorInfo.IME_FLAG_NO_ENTER_ACTION)
            assertEquals(0, info.inputType and InputType.TYPE_TEXT_FLAG_MULTI_LINE)

            assertTrue(connection.setComposingText("nihao", 1))
            assertTrue(connection.commitText("你好", 1))
            assertEquals(Input.Text("你好"), inputs.receive())
            for (text in listOf("\n", "第一行\n第二行\n")) {
                assertTrue(connection.commitText(text, 1))
                assertEquals(Input.Text(text), inputs.receive())
            }
            assertTrue(connection.setComposingText("\n", 1))
            assertTrue(connection.finishComposingText())
            assertEquals(Input.Text("\n"), inputs.receive())
            assertEditor(connection, "", 0, 0, -1, -1)

            // Both tested IMEs send a down/up pair for Return with this editor
            // configuration. Preserve the pair, including release semantics.
            assertTrue(connection.sendKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER)))
            assertEquals(Input.Key(NativeKey.Enter, 0, 1), inputs.receive())
            assertTrue(connection.sendKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_ENTER)))
            assertEquals(Input.Key(NativeKey.Enter, 0, 3), inputs.receive())

            var modifiers = 2
            view.pendingModifiers = { modifiers }
            view.consumedModifiers = { modifiers = 0 }
            assertTrue(connection.performEditorAction(EditorInfo.IME_ACTION_SEND))
            assertEquals(Input.Key(NativeKey.Enter, 2, 1), inputs.receive())
            assertEquals(0, modifiers)
            assertTrue(connection.performEditorAction(EditorInfo.IME_ACTION_SEND))
            assertEquals(Input.Key(NativeKey.Enter, 0, 1), inputs.receive())
            assertTrue(inputs.tryReceive().isFailure)
        }
    }

    @Test fun composingReplacementKeepsCursorQueriesAndSpansConsistent() {
        instrumentation.runOnMainSync {
            val view = TerminalView(instrumentation.targetContext).apply { update(frame(), 12) }
            val connection = view.onCreateInputConnection(EditorInfo()) as BaseInputConnection
            assertTrue(connection.setComposingText("n", 1))
            assertEquals("IME must see the Pinyin before its cursor", "n", connection.getTextBeforeCursor(16, 0).toString())
            assertEquals("", connection.getTextAfterCursor(16, 0).toString())
            assertEditor(connection, "n", 1, 1, 0, 1)
            assertTrue(connection.setComposingText("ni", 1))
            assertEditor(connection, "ni", 2, 2, 0, 2)

            assertTrue(connection.setComposingText("nihao", 0))
            assertEditor(connection, "nihao", 0, 0, 0, 5)
            assertTrue(connection.setSelection(2, 2))
            assertEquals("ni", connection.getTextBeforeCursor(16, 0).toString())
            assertEquals("hao", connection.getTextAfterCursor(16, 0).toString())
            val info = EditorInfo()
            view.onCreateInputConnection(info)
            assertEquals(2, info.initialSelStart); assertEquals(2, info.initialSelEnd)

            assertTrue(connection.setComposingRegion(2, 5))
            assertTrue(connection.setComposingText("好", 1))
            assertEditor(connection, "ni好", 3, 3, 2, 3)
            assertTrue(connection.setSelection(0, 2))
            assertEquals("ni", connection.getSelectedText(0).toString())
        }
    }

    @Test fun surroundingDeletionRespectsSelectionCompositionAndCodePoints() {
        instrumentation.runOnMainSync {
            val view = TerminalView(instrumentation.targetContext).apply { update(frame(), 12) }
            val connection = view.onCreateInputConnection(EditorInfo()) as BaseInputConnection
            assertTrue(connection.setComposingText("A😀BC", 1))
            // Android deletion excludes the selection and composing range. Here
            // C remains composing; the emoji before the cursor must be atomic.
            assertTrue(connection.setComposingRegion(4, 5))
            assertTrue(connection.setSelection(3, 3))
            assertTrue(connection.deleteSurroundingTextInCodePoints(1, 0))
            assertEditor(connection, "ABC", 1, 1, 2, 3)
            assertTrue(connection.setComposingRegion(1, 2))
            assertTrue(connection.deleteSurroundingText(1, 1))
            assertEditor(connection, "B", 0, 0, 0, 1)
            assertFalse(connection.deleteSurroundingText(-1, 0))
            assertFalse(connection.deleteSurroundingTextInCodePoints(129, 0))
        }
    }

    @Test fun rejectedCommitsAndRetiredConnectionsPreserveTheOwningBuffer() {
        instrumentation.runOnMainSync {
            val view = TerminalView(instrumentation.targetContext).apply { update(frame(), 12) }
            val connection = view.onCreateInputConnection(EditorInfo()) as BaseInputConnection
            assertTrue(connection.beginBatchEdit())
            assertTrue(connection.beginBatchEdit())
            assertTrue(connection.setComposingText("n😀", 1))
            assertTrue(connection.endBatchEdit())
            assertFalse(connection.endBatchEdit())
            assertFalse(connection.endBatchEdit())
            assertTrue(connection.setSelection(1, 1))
            // There is no Repository to admit text: the exact editor state must
            // survive both candidate commit and explicit finalization failure.
            assertFalse(connection.commitText("你", 1))
            assertFalse(connection.performEditorAction(EditorInfo.IME_ACTION_SEND))
            assertFalse(connection.finishComposingText())
            assertEditor(connection, "n😀", 1, 1, 0, 3)

            view.update(frame().copy(inputReady = false), 12)
            assertFalse(connection.setComposingText("lost", 1))
            assertFalse(connection.setSelection(0, 0))
            assertFalse(connection.setComposingRegion(0, 1))
            assertFalse(connection.performEditorAction(EditorInfo.IME_ACTION_SEND))
            assertEditor(connection, "n😀", 1, 1, 0, 3)

            view.update(frame().copy(inputEpoch = 2u), 12)
            val fresh = view.onCreateInputConnection(EditorInfo()) as BaseInputConnection
            assertEditor(fresh, "", 0, 0, -1, -1)
            assertTrue(fresh.setComposingText("new", 1))
            assertFalse(connection.commitText("old", 1))
            assertFalse(connection.performEditorAction(EditorInfo.IME_ACTION_SEND))
            assertFalse(connection.setSelection(0, 0))
            assertFalse(connection.setComposingRegion(0, 1))
            assertFalse(connection.deleteSurroundingText(1, 0))
            requireNotNull(connection.editable).append("retired")
            connection.closeConnection()
            assertEditor(fresh, "new", 3, 3, 0, 3)
        }
    }

    private sealed interface Input {
        data class Text(val text: String) : Input
        data class Key(val key: NativeKey, val modifiers: Int, val kind: Int) : Input
    }

    private suspend fun inputFixture(test: suspend (TerminalView, Channel<Input>) -> Unit) =
        withContext(Dispatchers.Main.immediate) {
            val context = instrumentation.targetContext
            val token = UUID.randomUUID().toString()
            val directory = File(context.cacheDir, "ime-enter-$token")
            val alias = "zterm.test.ime-enter.$token"
            val host = "1".repeat(64)
            val session = "2".repeat(32)
            val store = AppStore(context, directory, alias)
            withContext(Dispatchers.IO) {
                store.loadOrCreateSeed().fill(0)
                store.save(SavedState(hosts = listOf(SavedHost(host, "Fixture", emptyList(), session)),
                    activeTerminal = RecentConnection(host, session)))
            }
            val inputs = Channel<Input>(Channel.UNLIMITED)
            val runtime = object : NativeRuntime(NoHandle) {
                override suspend fun initialize(seed: ByteArray, dnsServers: List<NativeDnsServer>, hosts: List<NativeHost>) = "fixture"
                override suspend fun updateDns(servers: List<NativeDnsServer>) = Unit
                override suspend fun listSessions(host: String) = emptyList<NativeSession>()
                override suspend fun connectTerminal(host: String, session: String, viewport: NativeViewport, dark: Boolean, takeover: Boolean) =
                    object : NativeTerminal(NoHandle) {
                        override fun sessionId() = session
                        override fun currentFrame() = frame()
                        override suspend fun waitForFrame(afterGeneration: ULong): NativeFrame = awaitCancellation()
                        override suspend fun nextNotification(): NativeNotification = awaitCancellation()
                        override suspend fun detach() = Unit
                        override suspend fun commitText(inputEpoch: ULong, text: String, modifiers: UByte, paste: Boolean) {
                            inputs.send(Input.Text(text))
                        }
                        override suspend fun sendKey(inputEpoch: ULong, key: NativeKey, modifiers: UByte, kind: UByte, text: String) {
                            inputs.send(Input.Key(key, modifiers.toInt(), kind.toInt()))
                        }
                    }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
            val repository = AppRepository(context, runtime, scope = scope, store = store)
            val view = TerminalView(context)
            try {
                withTimeout(5_000) {
                    repository.state.first { it.initialized }
                    repository.activityStarted()
                    repository.frame.first { it?.inputReady == true }
                    view.repository = repository
                    test(view, inputs)
                }
            } finally {
                view.repository = null
                repository.goHome()
                withTimeout(5_000) { repository.state.first { it.route == Route.Home && it.hostId == null } }
                scope.coroutineContext.job.cancelAndJoin()
                runtime.close()
                inputs.close()
                withContext(Dispatchers.IO) {
                    directory.deleteRecursively()
                    KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(alias) }
                }
            }
        }

    private fun assertEditor(connection: BaseInputConnection, text: String, start: Int, end: Int, composingStart: Int, composingEnd: Int) {
        val editable = requireNotNull(connection.editable)
        assertEquals(text, editable.toString())
        assertEquals(start, Selection.getSelectionStart(editable))
        assertEquals(end, Selection.getSelectionEnd(editable))
        assertEquals(composingStart, BaseInputConnection.getComposingSpanStart(editable))
        assertEquals(composingEnd, BaseInputConnection.getComposingSpanEnd(editable))
    }

    private fun frame() = NativeFrame(
        applicationTitle = "",
        cursorShape = NativeCursorShape.BLOCK, cursorBlinking = false,
        connectionPath = NativeConnectionPath.UNKNOWN, rttMs = null,
        pointerMode = NativePointerMode.NONE, activeScreen = NativeActiveScreen.MAIN, source = null,
        stats = NativeNavigationStats(0u, 0u, 0u, 0u, 0u, 0u, 0u), notice = null,
        inputEpoch = 1u, geometryGeneration = 1u, inputReady = true,
        historyOffset = 0u, windowOffset = 0u, contentGeneration = 0u,
        firstRow = 0, selection = null, generation = 1u, state = "active", error = null,
        viewport = NativeViewport(3u, 16u), rows = emptyList(), cursorRow = 0u, cursorColumn = 0u,
        cursorVisible = true, cursorColor = 0xffffffffu, background = 0xff000000u, historyMaximum = 0u,
    )
}
