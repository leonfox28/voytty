package com.voytty.app

import android.view.WindowManager.LayoutParams
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalActivity
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.voytty.app.nativebridge.NativeKey
import com.voytty.app.nativebridge.NativeConnectionPath
import com.voytty.app.nativebridge.NativeSession

@OptIn(ExperimentalLayoutApi::class)
@Composable internal fun TerminalScreen(state: AppState, repository: AppRepository) {
    val frame by repository.terminalStatus.collectAsStateWithLifecycle()
    val upload by repository.uploads.state.collectAsStateWithLifecycle()
    var modifiers by remember { mutableIntStateOf(0) }
    var creating by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf<NativeSession?>(null) }
    var deleting by remember { mutableStateOf<NativeSession?>(null) }
    var taking by remember { mutableStateOf<String?>(null) }
    var cellHeight by remember { mutableIntStateOf(1) }
    var terminalView by remember { mutableStateOf<TerminalView?>(null) }
    val session = state.sessions.firstOrNull { it.sessionId == state.sessionId }
    val host = state.saved.hosts.firstOrNull { it.id == state.hostId }
    val imeVisible = WindowInsets.isImeVisible
    val imeAnimation = LocalImeAnimation.current
    val window = LocalActivity.current?.window
    DisposableEffect(window) {
        val previous = window?.attributes?.softInputMode
        // Keep editor focus for hardware input without Android auto-showing IME on task return.
        if (previous != null) window.setSoftInputMode(
            (previous and LayoutParams.SOFT_INPUT_MASK_STATE.inv()) or LayoutParams.SOFT_INPUT_STATE_UNCHANGED)
        onDispose {
            if (window != null && previous != null) window.setSoftInputMode(
                (window.attributes.softInputMode and LayoutParams.SOFT_INPUT_MASK_STATE.inv()) or
                    (previous and LayoutParams.SOFT_INPUT_MASK_STATE))
        }
    }
    DisposableEffect(terminalView, imeAnimation) {
        val stop = terminalView?.let { imeAnimation.observe(it::updateImeAnimation) }
        onDispose { stop?.invoke() }
    }
    BackHandler(enabled = state.panel || !imeVisible) { if (state.panel) repository.closePanel() else repository.goHome() }
    LaunchedEffect(frame?.inputEpoch, state.sessionId) { modifiers = 0 }
    val chrome = MaterialTheme.colorScheme.surfaceContainerLow
    val inputReady = frame?.inputReady == true
    var terminalHeight by remember { mutableIntStateOf(0) }
    val scrim = MaterialTheme.colorScheme.scrim.copy(alpha = .32f)
    val closeSheet = Modifier.clickable(indication = null, interactionSource = null) { repository.closePanel() }
    // The scrim also covers the status bar strip, which sits outside the padded content below.
    if (state.panel) Box(Modifier.fillMaxWidth().windowInsetsTopHeight(WindowInsets.statusBars).background(scrim).then(closeSheet))
    Box(Modifier.fillMaxSize().statusBarsPadding().navigationBarsPadding().imePadding()) {
        TerminalGridLayout(cellHeight, imeAnimation, Modifier.fillMaxSize(),
            onImeLayoutSettled = { terminalView?.imeLayoutSettled = it }) {
            Box(Modifier.fillMaxWidth().onSizeChanged { terminalHeight = it.height }) {
                AndroidView(factory = { context -> TerminalView(context).apply {
                        this.repository = repository; terminalView = this
                        onCellHeightChanged = { cellHeight = it }; onCellHeightChanged(gridCellHeight)
                    } },
                    modifier = Modifier.fillMaxSize(), onRelease = { terminalView = null }, update = { view ->
                        view.imeAnimating = { imeAnimation.running }
                        view.pendingModifiers = { modifiers }; view.consumedModifiers = { modifiers = 0 }
                        view.update(repository.frame.value, state.saved.preferences.fontSize)
                    })
                val terminalState = frame?.state
                val error = state.error ?: frame?.error
                val canTakeover = state.sessionId != null && (terminalState == "lease_lost" || error in setOf("session_occupied", "controller_busy", "lease_lost"))
                val inactive = terminalState in setOf("closed", "ended", "lease_lost") || state.error != null && frame == null
                if (inactive || frame == null && !state.panel) {
                    val title = stringResource(when {
                        state.busy -> R.string.connecting
                        canTakeover -> if (terminalState == "lease_lost") R.string.lease_lost else R.string.session_occupied
                        terminalState == "ended" || error in setOf("session_ended", "session_not_found") -> R.string.session_ended
                        else -> R.string.connection_failed_title
                    })
                    val body = if (state.busy) stringResource(R.string.connection_pending_body, host?.name ?: "voytty")
                        else if (error != null) errorMessage(error)
                        else stringResource(R.string.connection_failure_body)
                    WideStatusCard(title, body, Modifier.align(Alignment.Center).padding(24.dp),
                        icon = "disconnect", busy = state.busy) {
                        DialogActions {
                            if (state.busy && state.connecting) DialogButton(stringResource(R.string.cancel), repository::goHome)
                            else {
                                DialogButton(stringResource(R.string.sessions), repository::togglePanel, enabled = !state.busy)
                                DialogButton(stringResource(if (canTakeover) R.string.takeover else R.string.retry),
                                    { if (canTakeover) taking = state.sessionId else repository.retry() }, DialogRole.Primary, enabled = !state.busy)
                            }
                        }
                    }
                }
                if (terminalState == "reconnecting" || terminalState == "synchronizing" && frame?.inputReady != true) {
                    LinearProgressIndicator(Modifier.align(Alignment.TopCenter).fillMaxWidth())
                }
                val notice = state.error ?: frame?.notice
                if (notice != null && frame != null && !inactive) {
                    Surface(Modifier.align(Alignment.TopCenter).padding(8.dp), shape = MaterialTheme.shapes.small,
                        color = MaterialTheme.colorScheme.errorContainer, shadowElevation = 4.dp) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            ErrorText(notice, Modifier.padding(horizontal = 12.dp))
                            IconAction("close", stringResource(R.string.close)) { repository.clearError() }
                        }
                    }
                }
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            Row(Modifier.fillMaxWidth().height(TerminalToolbarHeight).background(chrome)
                .padding(horizontal = 4.dp, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(4.dp),
                verticalAlignment = Alignment.CenterVertically) {
                SessionChip(session?.name ?: host?.name ?: "voytty", connectionColor(frame, state.busy, state.error), state.panel,
                    repository::togglePanel, Modifier.fillMaxHeight().widthIn(SessionChipMinWidth, SessionChipMaxWidth))
                UploadToolbarButtons(repository.uploads, inputReady, Modifier.weight(1f).fillMaxHeight()) { terminalView?.requestFocus() }
                // Tab and Alt give way to the upload keys: legacy Ctrl+I is Tab, and an Esc prefix acts as Meta.
                val keys = listOf("Esc" to NativeKey.Escape, "Ctrl" to null,
                    "←" to NativeKey.Left, "↓" to NativeKey.Down, "↑" to NativeKey.Up, "→" to NativeKey.Right)
                keys.forEach { (label,key) ->
                    val bit = if (label == "Ctrl") 4 else 0
                    val arrow = when (key) {
                        NativeKey.Left -> "arrow-left" to R.string.key_left
                        NativeKey.Down -> "arrow-down" to R.string.key_down
                        NativeKey.Up -> "arrow-up" to R.string.key_up
                        NativeKey.Right -> "arrow-right" to R.string.key_right
                        else -> null
                    }
                    val description = arrow?.let { stringResource(it.second) }
                    ToolbarKey(onClick = {
                        if (bit != 0) modifiers = modifiers xor bit
                        else if (key != null) { repository.key(key,modifiers); repository.key(key,modifiers,3); modifiers = 0 }
                    }, enabled = inputReady && upload?.active != true,
                        modifier = Modifier.weight(1f).fillMaxHeight().semantics { if (description != null) contentDescription = description },
                        latched = bit != 0 && modifiers and bit != 0) {
                        if (arrow != null) LineIcon(arrow.first, Modifier.size(16.dp))
                        // Proportional type keeps "Esc" and "Ctrl" about as wide as each other and the icons.
                        else Text(label, fontSize = 13.sp, fontWeight = FontWeight.Medium, maxLines = 1)
                    }
                }
                val keyboardLabel = stringResource(if (imeVisible) R.string.hide_keyboard else R.string.show_keyboard)
                ToolbarKey(onClick = { terminalView?.setKeyboardVisible(!imeVisible) },
                    enabled = inputReady || imeVisible,
                    modifier = Modifier.weight(1f).fillMaxHeight().semantics { contentDescription = keyboardLabel },
                    latched = imeVisible) {
                    LineIcon("keyboard", Modifier.size(18.dp))
                }
            }
        }
        // The sheet is composed after the toolbar so its rows follow the session chip, and it is sized in
        // layout from the terminal area so keyboard animation frames never recompose it.
        if (state.panel) Box(Modifier.fillMaxWidth().layout { measurable, constraints ->
            val height = terminalHeight.coerceIn(0, constraints.maxHeight)
            val placeable = measurable.measure(Constraints.fixed(constraints.maxWidth, height))
            layout(constraints.maxWidth, height) { placeable.place(0, 0) }
        }) {
            Box(Modifier.fillMaxSize().background(scrim).then(closeSheet))
            Surface(Modifier.align(Alignment.BottomCenter).fillMaxWidth().layout { measurable, constraints ->
                    val limit = minOf((constraints.maxHeight * .65f).toInt(), 460.dp.roundToPx())
                    val placeable = measurable.measure(constraints.copy(minHeight = 0, maxHeight = limit))
                    layout(placeable.width, placeable.height) { placeable.place(0, 0) }
                }, color = chrome, shape = RoundedCornerShape(topStart = 24.dp, topEnd = 24.dp)) {
                Column {
                    Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                        Box(Modifier.weight(1f)) { TerminalConnectionSubtitle(host?.name.orEmpty(), frame, state.busy, state.error, 13.sp) }
                        IconAction("home", stringResource(R.string.back_to_hosts)) { repository.goHome() }
                    }
                    Box(Modifier.fillMaxWidth().height(3.dp)) { if (state.busy) LinearProgressIndicator(Modifier.fillMaxSize()) }
                    LazyColumn(Modifier.weight(1f, fill = false).padding(horizontal = 8.dp)) {
                        items(state.sessions, key = { it.sessionId }) { item ->
                            var menu by remember { mutableStateOf(false) }
                            // sessionId is also the failed/retry target; only a
                            // retained connection can own the current row.
                            val current = item.sessionId == state.sessionId && frame?.state in setOf("active", "synchronizing", "reconnecting")
                            val colors = MaterialTheme.colorScheme
                            Row(Modifier.fillMaxWidth().clip(MaterialTheme.shapes.medium)
                                .background(if (current) colors.primaryContainer else Color.Transparent)
                                .clickable(enabled = !state.busy) {
                                when {
                                    current -> repository.closePanel()
                                    item.occupied -> taking = item.sessionId
                                    else -> repository.selectSession(item.sessionId)
                                }
                            }.padding(start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                                IconTile("terminal", size = 36.dp,
                                    container = if (current) colors.primary else colors.surfaceContainerHigh,
                                    tint = if (current) colors.onPrimary else colors.onSurfaceVariant)
                                Column(Modifier.weight(1f).padding(horizontal = 12.dp, vertical = 12.dp)) {
                                    Text(item.name, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyLarge,
                                        fontWeight = if (current) FontWeight.SemiBold else FontWeight.Normal,
                                        color = if (current) colors.onPrimaryContainer else colors.onSurface)
                                    if (current || item.occupied) Row {
                                        val secondary = if (current) colors.onPrimaryContainer.copy(alpha = .72f) else LocalVoyttyColors.current.warning
                                        Text(stringResource(if(current) R.string.current_session else R.string.occupied),
                                            style = MaterialTheme.typography.bodySmall, color = secondary)
                                        if (current) terminalApplicationTitle(item.name, frame?.applicationTitle.orEmpty())?.let { title ->
                                            Text(" · $title", Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
                                                style = MaterialTheme.typography.bodySmall, color = secondary)
                                        }
                                    }
                                }
                                Box {
                                    IconAction("more", stringResource(R.string.session_menu), !state.busy) { menu = true }
                                    DropdownMenu(menu, { menu = false }, shape = MaterialTheme.shapes.medium) {
                                        DropdownMenuItem(text = { Text(stringResource(R.string.rename)) }, onClick = { menu = false; renaming = item })
                                        DropdownMenuItem(text = { Text(stringResource(R.string.delete), color = colors.error) }, onClick = { menu = false; deleting = item })
                                    }
                                }
                            }
                        }
                    }
                    ErrorText(state.error, Modifier.padding(horizontal = 20.dp))
                    state.sessionListError?.let { code ->
                        SessionListError(code, !state.sessionsRefreshing, repository::refreshSessionList)
                    }
                    FilledTonalButton({ creating = true }, Modifier.fillMaxWidth().padding(12.dp).height(48.dp), enabled = !state.busy,
                        shape = MaterialTheme.shapes.medium) {
                        LineIcon("plus", Modifier.size(20.dp)); Spacer(Modifier.width(8.dp)); Text(stringResource(R.string.new_session), maxLines = 1)
                    }
                }
            }
        }
    }
    if (creating) SessionForm(null, state.busy, state.error, { creating = false; repository.clearError() }) { name,directory -> repository.createSession(name,directory) }
    renaming?.let { row -> SessionForm(row.name, state.busy, state.error, { renaming = null; repository.clearError() }) { name,_ -> repository.renameSession(row.sessionId,name) } }
    LaunchedEffect(state.busy, state.error, state.sessionId, state.sessions) {
        if (!state.busy && state.error == null) {
            if (frame != null) creating = false
            renaming?.let { old -> if (state.sessions.any { it.sessionId == old.sessionId && it.name != old.name }) renaming = null }
        }
    }
    deleting?.let { row -> ConfirmDialog(stringResource(R.string.delete_session), row.name + "\n" + stringResource(R.string.delete_session_message), stringResource(R.string.delete), { deleting = null },
        icon = "trash", destructive = true) { deleting = null; repository.deleteSession(row.sessionId) } }
    taking?.let { id ->
        val name = state.sessions.firstOrNull { it.sessionId == id }?.name
        ConfirmDialog(stringResource(R.string.takeover), name?.let { "$it\n" }.orEmpty() + stringResource(R.string.takeover_message), stringResource(R.string.takeover), { taking = null }, icon = "terminal") {
            taking = null; repository.selectSession(id,true)
        }
    }
}

@Composable internal fun SessionListError(code: String, enabled: Boolean, retry: () -> Unit) {
    Column(Modifier.padding(horizontal = 20.dp)) {
        Text(stringResource(R.string.session_list_refresh_failed), color = MaterialTheme.colorScheme.error)
        ErrorText(code)
        TextButton(retry, enabled = enabled) { Text(stringResource(R.string.retry)) }
    }
}
@Composable internal fun connectionColor(status: TerminalStatus?, busy: Boolean, error: String?): Color {
    val extra = LocalVoyttyColors.current
    return when (status?.state) {
        "active" -> extra.success
        "synchronizing" -> if (status.inputReady) extra.success else extra.warning
        "reconnecting" -> extra.warning
        "ended", "lease_lost", "closed" -> MaterialTheme.colorScheme.error
        else -> if (busy) extra.warning else if (error != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.outline
    }
}
@Composable internal fun TerminalConnectionSubtitle(host: String, status: TerminalStatus?, busy: Boolean, error: String?,
    fontSize: TextUnit = 11.sp) {
    val label = when (status?.state) {
        "reconnecting" -> stringResource(R.string.reconnecting)
        "ended" -> stringResource(R.string.session_ended)
        "lease_lost" -> stringResource(R.string.lease_lost)
        "closed" -> stringResource(R.string.disconnected)
        "active", "synchronizing" -> {
            if (status.state == "synchronizing" && status.connectionPath == NativeConnectionPath.UNKNOWN && !status.inputReady) {
                stringResource(R.string.connecting)
            } else {
                val route = stringResource(when (status.connectionPath) {
                    NativeConnectionPath.DIRECT -> R.string.connection_direct
                    NativeConnectionPath.RELAY -> R.string.connection_relay
                    NativeConnectionPath.UNKNOWN -> R.string.connected
                })
                val latency = stringResource(R.string.connection_latency, status.rttMs?.toString() ?: "—")
                "$route · $latency"
            }
        }
        else -> stringResource(if (busy) R.string.connecting else if (error != null) R.string.connection_error else R.string.disconnected)
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        StatusDot(connectionColor(status, busy, error), Modifier.padding(end = 6.dp).size(if (fontSize > 11.sp) 8.dp else 6.dp))
        Text(host, Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
            fontSize = fontSize, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(" · $label", maxLines = 1, overflow = TextOverflow.Ellipsis,
            fontSize = fontSize, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
internal val TerminalToolbarHeight = 48.dp
// The chip follows its session name between these bounds: the minimum fits the dot and four characters, and
// the maximum keeps the other nine keys at least 32dp wide on a 412dp phone; longer names end in an ellipsis.
private val SessionChipMinWidth = 56.dp
private val SessionChipMaxWidth = 80.dp

/** Stands in for a title bar: connection health and the session name, sized to its content; opens the session sheet. */
@Composable private fun SessionChip(name: String, health: Color, open: Boolean, onClick: () -> Unit, modifier: Modifier) {
    ToolbarKey(onClick, enabled = true, modifier = modifier, latched = open, filled = true) {
        Row(Modifier.padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            StatusDot(health, Modifier.size(6.dp))
            Text(name, Modifier.padding(start = 6.dp).weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
                fontSize = 12.sp, fontWeight = FontWeight.Medium)
        }
    }
}

/**
 * A compact toolbar button. Keys are flat so the row reads as one strip; only [filled] controls (the session
 * chip) keep a key-cap fill, and [latched] marks a held modifier or another active state.
 */
@Composable internal fun ToolbarKey(onClick: () -> Unit, enabled: Boolean, modifier: Modifier, latched: Boolean = false,
    filled: Boolean = false, content: @Composable RowScope.() -> Unit) {
    val extra = LocalVoyttyColors.current
    val rest = if (filled) extra.keyCap else Color.Transparent
    TextButton(onClick = onClick, enabled = enabled, modifier = modifier, shape = MaterialTheme.shapes.extraSmall,
        contentPadding = PaddingValues(0.dp),
        colors = ButtonDefaults.textButtonColors(
            containerColor = if (latched) MaterialTheme.colorScheme.primaryContainer else rest,
            contentColor = if (latched) MaterialTheme.colorScheme.onPrimaryContainer else extra.onKeyCap,
            disabledContainerColor = if (filled) rest.copy(alpha = .5f) else rest,
            disabledContentColor = extra.onKeyCap.copy(alpha = .38f)),
        content = content)
}
/** Children: terminal, divider, toolbar. The unoccupied remainder is below the fixed toolbar, inside the already consumed insets. */
@OptIn(ExperimentalLayoutApi::class)
@Composable internal fun TerminalGridLayout(cellHeight: Int, imeAnimation: ImeAnimationState, modifier: Modifier,
    onImeLayoutSettled: (Boolean) -> Unit = {},
    content: @Composable () -> Unit) {
    val density = LocalDensity.current
    val ime = WindowInsets.ime
    val start = WindowInsets.imeAnimationSource
    val end = WindowInsets.imeAnimationTarget
    val navigation = WindowInsets.navigationBars
    Layout(content = content, modifier = modifier) { children, constraints ->
        val chromeConstraints = constraints.copy(minHeight = 0)
        val divider = children[1].measure(chromeConstraints)
        val toolbar = children[2].measure(chromeConstraints)
        val available = (constraints.maxHeight - divider.height - toolbar.height).coerceAtLeast(0)
        val nav = navigation.getBottom(density)
        val currentBottom = maxOf(nav, ime.getBottom(density))
        val startBottom = maxOf(nav, start.getBottom(density))
        val endBottom = maxOf(nav, end.getBottom(density))
        // onEnd may reach the native View before Compose has consumed the final
        // inset. Only the layout measured at the target inset may be submitted.
        onImeLayoutSettled(currentBottom == endBottom)
        val remainder = terminalBottomRemainder(available, cellHeight, currentBottom,
            startBottom, endBottom, imeAnimation.running)
        val terminal = children[0].measure(Constraints.fixed(constraints.maxWidth, available - remainder))
        layout(constraints.maxWidth, constraints.maxHeight) {
            terminal.placeRelative(0, 0)
            divider.placeRelative(0, terminal.height)
            toolbar.placeRelative(0, terminal.height + divider.height)
        }
    }
}
@Composable private fun SessionForm(previous: String?, busy: Boolean, error: String?, dismiss: () -> Unit, submit: (String,String) -> Unit) {
    var name by remember(previous) { mutableStateOf(previous.orEmpty()) }
    var directory by remember { mutableStateOf("") }
    VoyttyDialog(dismiss, stringResource(if (previous == null) R.string.new_session else R.string.rename),
        icon = if (previous == null) "plus" else "edit", actions = {
            DialogButton(stringResource(R.string.cancel), dismiss)
            DialogButton(stringResource(if (previous == null) R.string.create else R.string.save), { submit(name,directory) },
                DialogRole.Primary, enabled = name.isNotBlank() && !busy, busy = busy)
        }) {
        OutlinedTextField(name, { if (it.length <= 128) name = it }, Modifier.fillMaxWidth(), label = { Text(stringResource(R.string.name)) },
            singleLine = true, enabled = !busy, shape = MaterialTheme.shapes.medium)
        if (previous == null) OutlinedTextField(directory, { if (it.length <= 4096) directory = it }, Modifier.fillMaxWidth(),
            label = { Text(stringResource(R.string.directory_optional)) }, singleLine = true, enabled = !busy, shape = MaterialTheme.shapes.medium)
        ErrorText(error)
    }
}

/** The running program's terminal title, unless it is empty or merely repeats the session name. */
internal fun terminalApplicationTitle(sessionName: String, applicationTitle: String): String? =
    applicationTitle.takeUnless { it.isEmpty() || it == sessionName }
