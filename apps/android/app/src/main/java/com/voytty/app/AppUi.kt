package com.voytty.app

import android.content.res.Configuration
import android.os.LocaleList
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.voytty.app.nativebridge.NativeSession
import kotlin.math.roundToInt
import java.util.Locale

@Composable internal fun VoyttyApp(repository: AppRepository, updates: AppUpdates) {
    val state by repository.state.collectAsStateWithLifecycle()
    val system = LocalConfiguration.current
    val context = LocalContext.current
    val language = state.saved.preferences.language
    val updateBlocks = remember { mutableIntStateOf(0) }
    val localized = remember(context, system, language) {
        val configuration = Configuration(system)
        if (language != "system") configuration.setLocales(LocaleList(Locale.forLanguageTag(language)))
        // Preserve the Activity in the wrapper chain for launchers, dialogs and
        // CameraX while giving resources an independent language configuration.
        android.view.ContextThemeWrapper(context, context.theme).apply { applyOverrideConfiguration(configuration) }
    }
    val dark = when (state.saved.preferences.theme) { "dark" -> true; "light" -> false; else -> isSystemInDarkTheme() }
    LaunchedEffect(dark) {
        repository.setDark(dark)
        (context as? android.app.Activity)?.window?.let { window ->
            androidx.core.view.WindowCompat.getInsetsController(window, window.decorView).apply {
                isAppearanceLightStatusBars = !dark
                isAppearanceLightNavigationBars = !dark
            }
        }
    }
    CompositionLocalProvider(LocalContext provides localized, LocalConfiguration provides localized.resources.configuration,
        LocalUpdateBlocks provides updateBlocks) {
        VoyttyTheme(dark) {
            Surface(Modifier.fillMaxSize()) {
                Box {
                    when (state.route) {
                        Route.Home -> HomeScreen(state, repository)
                        Route.Settings -> SettingsScreen(state, repository, updates)
                        Route.Scanner -> ScannerScreen(state, repository)
                        Route.Terminal -> TerminalScreen(state, repository)
                    }
                    AppUpdateHost(updates, state)
                }
            }
        }
    }
}

@Composable internal fun LineIcon(name: String, modifier: Modifier = Modifier.size(24.dp), color: Color = LocalContentColor.current) {
    Canvas(modifier) {
        val factor = size.width / 24
        val stroke = 1.7f * factor
        fun line(x: Float, y: Float, a: Float, b: Float) = drawLine(color, Offset(x * factor, y * factor), Offset(a * factor, b * factor), stroke)
        fun circle(x: Float, y: Float, radius: Float) = drawCircle(color, radius * factor, Offset(x * factor, y * factor), style = Stroke(stroke))
        when (name) {
            "back" -> { line(15f,5f,8f,12f); line(8f,12f,15f,19f) }
            "right" -> { line(9f,5f,16f,12f); line(16f,12f,9f,19f) }
            "close" -> { line(6f,6f,18f,18f); line(18f,6f,6f,18f) }
            "down" -> { line(6f,9f,12f,15f); line(12f,15f,18f,9f) }
            "up" -> { line(6f,15f,12f,9f); line(12f,9f,18f,15f) }
            "home" -> { line(3f,11f,12f,3.5f); line(12f,3.5f,21f,11f); line(5.5f,9f,5.5f,20f); line(5.5f,20f,18.5f,20f)
                line(18.5f,20f,18.5f,9f); line(10f,20f,10f,14f); line(10f,14f,14f,14f); line(14f,14f,14f,20f) }
            "trash" -> { line(4f,7f,20f,7f); line(9f,7f,9f,4f); line(9f,4f,15f,4f); line(15f,4f,15f,7f)
                line(6f,7f,7f,20f); line(7f,20f,17f,20f); line(17f,20f,18f,7f); line(10f,11f,10f,16f); line(14f,11f,14f,16f) }
            "edit" -> { line(4f,20f,4.5f,16f); line(4f,20f,8f,19.5f); line(4.5f,16f,15.5f,5f); line(8f,19.5f,19f,8.5f)
                line(15.5f,5f,19f,8.5f); line(13f,7.5f,16.5f,11f) }
            "bell" -> {
                val path = Path().apply {
                    moveTo(6f*factor,17f*factor); lineTo(6f*factor,11f*factor)
                    cubicTo(6f*factor,7.7f*factor,8.7f*factor,5f*factor,12f*factor,5f*factor)
                    cubicTo(15.3f*factor,5f*factor,18f*factor,7.7f*factor,18f*factor,11f*factor); lineTo(18f*factor,17f*factor)
                }
                drawPath(path,color,style=Stroke(stroke)); line(4f,17f,20f,17f); line(10f,20f,14f,20f); line(12f,3f,12f,5f)
            }
            "plus" -> { line(12f,5f,12f,19f); line(5f,12f,19f,12f) }
            "arrow-left" -> { line(21f,12f,3f,12f); line(3f,12f,10f,5f); line(3f,12f,10f,19f) }
            "arrow-down" -> { line(12f,3f,12f,21f); line(12f,21f,5f,14f); line(12f,21f,19f,14f) }
            "arrow-up" -> { line(12f,21f,12f,3f); line(12f,3f,5f,10f); line(12f,3f,19f,10f) }
            "arrow-right" -> { line(3f,12f,21f,12f); line(21f,12f,14f,5f); line(21f,12f,14f,19f) }
            "more" -> for (y in listOf(5f,12f,19f)) drawCircle(color, 1.5f*factor, Offset(12f*factor,y*factor))
            "terminal" -> { line(4f,6f,10f,12f); line(10f,12f,4f,18f); line(13f,18f,21f,18f) }
            "download" -> { line(12f,3f,12f,15f); line(7f,10f,12f,15f); line(12f,15f,17f,10f); line(4f,16f,4f,21f); line(4f,21f,20f,21f); line(20f,21f,20f,16f) }
            "external" -> { line(14f,3f,21f,3f); line(21f,3f,21f,10f); line(21f,3f,10f,14f); line(10f,3f,3f,3f); line(3f,3f,3f,21f); line(3f,21f,21f,21f); line(21f,21f,21f,14f) }
            "disconnect" -> { circle(12f,12f,9f); line(8f,8f,16f,16f); line(16f,8f,8f,16f) }
            "attachment" -> {
                val path = Path().apply {
                    moveTo(21f*factor, 11f*factor)
                    lineTo(12f*factor, 20f*factor)
                    cubicTo(9.8f*factor,22.2f*factor,6.2f*factor,22.2f*factor,4f*factor,20f*factor)
                    cubicTo(1.8f*factor,17.8f*factor,1.8f*factor,14.2f*factor,4f*factor,12f*factor)
                    lineTo(13f*factor, 3f*factor)
                    cubicTo(14.4f*factor,1.6f*factor,16.6f*factor,1.6f*factor,18f*factor,3f*factor)
                    cubicTo(19.4f*factor,4.4f*factor,19.4f*factor,6.6f*factor,18f*factor,8f*factor)
                    lineTo(9f*factor,17f*factor)
                    cubicTo(8.3f*factor,17.7f*factor,7.2f*factor,17.7f*factor,6.5f*factor,17f*factor)
                    cubicTo(5.8f*factor,16.3f*factor,5.8f*factor,15.2f*factor,6.5f*factor,14.5f*factor)
                    lineTo(15f*factor,6f*factor)
                }
                drawPath(path,color,style=Stroke(stroke, cap=androidx.compose.ui.graphics.StrokeCap.Round, join=androidx.compose.ui.graphics.StrokeJoin.Round))
            }
            "keyboard" -> {
                drawRoundRect(color, Offset(2f*factor,5f*factor), androidx.compose.ui.geometry.Size(20f*factor,14f*factor), androidx.compose.ui.geometry.CornerRadius(2f*factor), style=Stroke(stroke))
                for (y in listOf(9f,12f)) for (x in listOf(6f,10f,14f,18f)) line(x,y,x+.5f,y)
                line(7f,16f,17f,16f)
            }
            "settings" -> {
                val p = Path()
                for (i in 0..32) {
                    val angle = i * Math.PI / 16
                    val radius = if (i % 4 in 1..2) 10f else 8f
                    val x = (12 + kotlin.math.cos(angle)*radius).toFloat()*factor
                    val y = (12 + kotlin.math.sin(angle)*radius).toFloat()*factor
                    if (i == 0) p.moveTo(x,y) else p.lineTo(x,y)
                }; p.close(); drawPath(p,color,style=Stroke(stroke)); circle(12f,12f,3f)
            }
            "image" -> { drawRoundRect(color, Offset(3f*factor,3f*factor), androidx.compose.ui.geometry.Size(18f*factor,18f*factor), androidx.compose.ui.geometry.CornerRadius(2f*factor), style=Stroke(stroke)); circle(8f,8f,1.5f); line(4f,18f,10f,12f); line(10f,12f,14f,16f); line(14f,16f,18f,11f); line(18f,11f,21f,14f) }
            else -> {
                for (y in listOf(4f,14f)) {
                    drawRoundRect(color, Offset(3f*factor,y*factor), androidx.compose.ui.geometry.Size(18f*factor,6f*factor), androidx.compose.ui.geometry.CornerRadius(factor), style=Stroke(stroke))
                    line(6f,y+3,8f,y+3)
                }
            }
        }
    }
}
@Composable internal fun IconAction(icon: String, label: String, enabled: Boolean = true, onClick: () -> Unit) {
    IconButton(onClick, enabled = enabled) {
        Box(Modifier.semanticsDescription(label)) { LineIcon(icon) }
    }
}
private fun Modifier.semanticsDescription(label: String): Modifier = this.then(Modifier.semantics { contentDescription = label })

@Composable private fun HomeScreen(state: AppState, repository: AppRepository) {
    var removing by remember { mutableStateOf<SavedHost?>(null) }
    val colors = MaterialTheme.colorScheme
    Column(Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 20.dp)) {
        Row(Modifier.fillMaxWidth().height(72.dp), verticalAlignment = Alignment.CenterVertically) {
            IconTile("terminal", container = colors.primaryContainer, tint = colors.onPrimaryContainer)
            Text("voytty", Modifier.padding(start = 14.dp).weight(1f), style = MaterialTheme.typography.headlineSmall)
            IconAction("settings", stringResource(R.string.settings)) { repository.show(Route.Settings) }
        }
        Box(Modifier.fillMaxWidth().height(4.dp)) {
            if (state.busy || !state.initialized && state.error == null)
                LinearProgressIndicator(Modifier.fillMaxSize().clip(CircleShape))
        }
        val hosts = state.saved.hosts
        if (state.initialized && hosts.isEmpty()) EmptyHosts(Modifier.weight(1f).fillMaxWidth())
        else LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp), contentPadding = PaddingValues(top = 12.dp, bottom = 96.dp)) {
            val recent = state.saved.recent
            val recentHost = hosts.firstOrNull { it.id == recent?.host }
            if (recent != null && recentHost != null) item {
                RecentCard(recentHost.name, !state.busy) { repository.connectHost(recent.host, true) }
            }
            item { SectionLabel(stringResource(R.string.saved_hosts)) }
            items(hosts, key = { it.id }) { host ->
                Surface(Modifier.fillMaxWidth(), shape = MaterialTheme.shapes.medium, color = colors.surfaceContainer) {
                    Row(Modifier.combinedClickable(enabled = !state.busy,
                        onClick = { repository.connectHost(host.id) }, onLongClick = { removing = host })
                        .padding(horizontal = 16.dp, vertical = 14.dp), verticalAlignment = Alignment.CenterVertically) {
                        HostMonogram(host.name)
                        Text(host.name, Modifier.padding(horizontal = 16.dp).weight(1f), style = MaterialTheme.typography.bodyLarge,
                            fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        LineIcon("right", Modifier.size(20.dp), colors.onSurfaceVariant)
                    }
                }
            }
        }
        // Leave room so the floating add button never covers the error line.
        ErrorText(state.error, Modifier.padding(end = 88.dp, bottom = 16.dp))
    }
    val enabled = state.initialized && !state.busy
    val addLabel = stringResource(R.string.add_host)
    Box(Modifier.fillMaxSize().safeDrawingPadding().padding(20.dp), contentAlignment = Alignment.BottomEnd) {
        Surface({ repository.show(Route.Scanner) }, Modifier.size(64.dp).semantics { contentDescription = addLabel },
            enabled = enabled, shape = CircleShape, shadowElevation = 6.dp,
            color = if (enabled) colors.primary else colors.surfaceContainerHighest,
            contentColor = if (enabled) colors.onPrimary else colors.onSurfaceVariant) {
            Box(contentAlignment = Alignment.Center) { LineIcon("plus", Modifier.size(28.dp)) }
        }
    }
    removing?.let { host -> ConfirmDialog(stringResource(R.string.remove_host), host.name + "\n" + stringResource(R.string.remove_host_message),
        stringResource(R.string.remove), { removing = null }, icon = "trash", destructive = true) { removing = null; repository.removeHost(host.id) } }
}
@Composable private fun RecentCard(name: String, enabled: Boolean, open: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Surface(open, Modifier.fillMaxWidth(), enabled = enabled, shape = MaterialTheme.shapes.large, color = colors.primaryContainer,
        contentColor = colors.onPrimaryContainer) {
        Row(Modifier.padding(20.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(stringResource(R.string.recent_connection), style = MaterialTheme.typography.labelMedium,
                    color = colors.onPrimaryContainer.copy(alpha = .72f))
                Text(name, Modifier.padding(top = 6.dp), style = MaterialTheme.typography.headlineSmall,
                    maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(stringResource(R.string.resume_session), Modifier.padding(top = 2.dp), style = MaterialTheme.typography.bodyMedium,
                    color = colors.onPrimaryContainer.copy(alpha = .72f))
            }
            Box(Modifier.size(48.dp).clip(CircleShape).background(colors.primary), contentAlignment = Alignment.Center) {
                LineIcon("arrow-right", Modifier.size(22.dp), colors.onPrimary)
            }
        }
    }
}
@Composable private fun HostMonogram(name: String) {
    Box(Modifier.size(40.dp).clip(MaterialTheme.shapes.small).background(MaterialTheme.colorScheme.surfaceContainerHigh),
        contentAlignment = Alignment.Center) {
        Text(name.trim().firstOrNull()?.uppercase() ?: "#", style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.primary, fontFamily = TerminalFont.family)
    }
}
@Composable private fun EmptyHosts(modifier: Modifier) {
    Column(modifier.padding(horizontal = 12.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        IconTile("server", size = 72.dp, tint = MaterialTheme.colorScheme.primary)
        Text(stringResource(R.string.home_empty_title), Modifier.padding(top = 24.dp), style = MaterialTheme.typography.titleLarge)
        Text(stringResource(R.string.home_empty_body), Modifier.padding(top = 8.dp), style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
        CommandChip("voytty pair create", Modifier.padding(top = 20.dp))
    }
}
@Composable internal fun CommandChip(command: String, modifier: Modifier = Modifier) {
    Surface(modifier, shape = MaterialTheme.shapes.small, color = MaterialTheme.colorScheme.surfaceContainerHigh) {
        Text("$ $command", Modifier.padding(horizontal = 14.dp, vertical = 8.dp), fontFamily = TerminalFont.family,
            fontSize = 14.sp, color = MaterialTheme.colorScheme.onSurface)
    }
}
@Composable private fun SettingsScreen(state: AppState, repository: AppRepository, updates: AppUpdates) {
    BackHandler { repository.show(Route.Home) }
    val preferences = state.saved.preferences
    var fontSize by remember(preferences.fontSize) { mutableIntStateOf(preferences.fontSize) }
    Column(Modifier.fillMaxSize().safeDrawingPadding()) {
        PageHeader(stringResource(R.string.settings)) { repository.show(Route.Home) }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
            SettingsGroup(stringResource(R.string.general)) {
                PreferenceChoice(stringResource(R.string.language), preferences.language,
                    listOf("system" to stringResource(R.string.follow_system), "zh" to stringResource(R.string.chinese), "en" to stringResource(R.string.english))) {
                    val value = it
                    repository.updatePreferences { current -> current.copy(language = value) }
                }
                GroupDivider()
                PreferenceChoice(stringResource(R.string.theme), preferences.theme,
                    listOf("system" to stringResource(R.string.follow_system), "dark" to stringResource(R.string.dark), "light" to stringResource(R.string.light))) {
                    val value = it
                    repository.updatePreferences { current -> current.copy(theme = value) }
                }
            }
            SettingsGroup(stringResource(R.string.terminal_section)) {
                val fontLabel = stringResource(R.string.font_size)
                Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(fontLabel, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                    Surface(shape = MaterialTheme.shapes.extraSmall, color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                        Text(fontSize.toString(), Modifier.padding(horizontal = 10.dp, vertical = 4.dp), fontFamily = TerminalFont.family,
                            style = MaterialTheme.typography.bodyMedium)
                    }
                }
                Slider(value = fontSize.toFloat(),
                    onValueChange = { fontSize = it.roundToInt().coerceIn(terminalFontSizes) },
                    onValueChangeFinished = {
                        val value = fontSize
                        repository.updatePreferences { current -> current.copy(fontSize = value) }
                    },
                    valueRange = terminalFontSizes.first.toFloat()..terminalFontSizes.last.toFloat(),
                    steps = terminalFontSizes.last - terminalFontSizes.first - 1,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).semantics { contentDescription = fontLabel })
                Surface(Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, bottom = 12.dp),
                    color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.medium) {
                    Text(stringResource(R.string.font_preview), Modifier.padding(16.dp), fontFamily = TerminalFont.family, fontSize = fontSize.sp)
                }
            }
            BackgroundConnectionSettings(state, repository)
            TerminalNotificationSettings(state, repository)
            DiagnosticsSettings()
            AboutSettings(updates, state.initialized)
            ErrorText(state.error)
        }
    }
}
@Composable private fun PreferenceChoice(title: String, selected: String, options: List<Pair<String,String>>, change: (String) -> Unit) {
    Column(Modifier.fillMaxWidth().padding(16.dp)) {
        Text(title, Modifier.padding(bottom = 12.dp), style = MaterialTheme.typography.bodyLarge)
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            options.forEachIndexed { index, (value, label) ->
                SegmentedButton(selected == value, onClick = { change(value) }, SegmentedButtonDefaults.itemShape(index, options.size),
                    colors = SegmentedButtonDefaults.colors(activeContainerColor = MaterialTheme.colorScheme.primaryContainer,
                        activeContentColor = MaterialTheme.colorScheme.onPrimaryContainer, activeBorderColor = MaterialTheme.colorScheme.outline,
                        inactiveContainerColor = MaterialTheme.colorScheme.surfaceContainer, inactiveBorderColor = MaterialTheme.colorScheme.outline),
                    icon = {}, label = { Text(label, maxLines = 1) })
            }
        }
    }
}
@Composable internal fun ErrorText(code: String?, modifier: Modifier = Modifier) {
    if (code != null) Text(errorMessage(code), modifier.padding(vertical = 8.dp), color = MaterialTheme.colorScheme.error, fontSize = 13.sp)
}
@Composable internal fun errorMessage(code: String): String = stringResource(when (code) {
    "invalid_ticket", "pair_ticket_invalid", "pair_ticket_consumed", "pairing_invalid_ticket", "ticket_invalid" -> R.string.invalid_ticket
    "ticket_expired", "pair_ticket_expired", "pairing_expired", "pairing_ticket_expired" -> R.string.ticket_expired
    "identity_state_mismatch", "identity_invalid" -> R.string.identity_error
    "storage_unavailable" -> R.string.storage_error
    "unauthorized", "authorization_revoked", "device_revoked", "peer_not_authorized" -> R.string.unauthorized
    "session_not_found", "session_ended" -> R.string.session_ended
    "session_occupied", "controller_busy" -> R.string.session_occupied
    "lease_lost" -> R.string.lease_lost
    "invalid_session_name" -> R.string.invalid_name
    "session_name_conflict", "session_already_exists" -> R.string.duplicate_name
    "invalid_working_directory", "working_directory_unavailable" -> R.string.invalid_directory
    "operation_outcome_unknown", "pair_outcome_unknown" -> R.string.outcome_unknown
    "resource_limit", "resource_exhausted" -> R.string.resource_limit
    "input_not_ready" -> R.string.input_unavailable
    "upload_too_large" -> R.string.upload_too_large
    "upload_source_invalid", "upload_already_started" -> R.string.upload_source_invalid
    "upload_storage_failed" -> R.string.upload_storage_failed
    "upload_outcome_unknown" -> R.string.upload_outcome_unknown
    "service_not_implemented" -> R.string.upload_upgrade
    "no_qr" -> R.string.no_qr
    "selection_changed" -> R.string.selection_changed
    "history_unavailable" -> R.string.history_unavailable
    "image_unreadable" -> R.string.image_unreadable
    "camera_unavailable" -> R.string.camera_unavailable
    else -> R.string.connection_error
})
