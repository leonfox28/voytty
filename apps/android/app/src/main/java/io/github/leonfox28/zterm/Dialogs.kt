package io.github.leonfox28.zterm

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties

/** Visual weight of a dialog action. */
internal enum class DialogRole { Secondary, Primary, Destructive }

private val DialogButtonHeight = 48.dp
private val DialogPadding = 24.dp

@Composable internal fun DialogSurface(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Surface(modifier.widthIn(max = 560.dp).fillMaxWidth(), shape = MaterialTheme.shapes.extraLarge,
        color = MaterialTheme.colorScheme.surfaceContainer, content = content)
}

@Composable private fun DialogHeader(title: String, body: String?, icon: String?, busy: Boolean, destructive: Boolean) {
    val colors = MaterialTheme.colorScheme
    if (icon != null || busy) {
        val container = if (destructive) colors.errorContainer else colors.primaryContainer
        val tint = if (destructive) colors.onErrorContainer else colors.onPrimaryContainer
        if (busy) Surface(Modifier.size(48.dp), shape = MaterialTheme.shapes.medium, color = container) {
            Box(contentAlignment = Alignment.Center) { CircularProgressIndicator(Modifier.size(24.dp), tint, strokeWidth = 2.dp) }
        } else IconTile(icon!!, size = 48.dp, container = container, tint = tint)
        Spacer(Modifier.height(16.dp))
    }
    Text(title, style = MaterialTheme.typography.titleLarge)
    if (body != null) Text(body, Modifier.padding(top = 8.dp), style = MaterialTheme.typography.bodyMedium, color = colors.onSurfaceVariant)
}

@Composable internal fun DialogButton(text: String, onClick: () -> Unit, role: DialogRole = DialogRole.Secondary,
    enabled: Boolean = true, busy: Boolean = false) {
    val modifier = Modifier.heightIn(min = DialogButtonHeight)
    val label: @Composable RowScope.() -> Unit = {
        if (busy) CircularProgressIndicator(Modifier.size(20.dp), LocalContentColor.current, strokeWidth = 2.dp)
        else Text(text, textAlign = TextAlign.Center, maxLines = 2)
    }
    when (role) {
        DialogRole.Secondary -> FilledTonalButton(onClick, modifier, enabled, content = label)
        DialogRole.Primary -> Button(onClick, modifier, enabled, content = label)
        DialogRole.Destructive -> Button(onClick, modifier, enabled, content = label, colors = ButtonDefaults.buttonColors(
            containerColor = MaterialTheme.colorScheme.error, contentColor = MaterialTheme.colorScheme.onError))
    }
}

/**
 * Places actions side by side at equal widths with the primary action last. When any label would not fit its
 * share, the actions stack at full width with the primary action on top.
 */
@Composable internal fun DialogActions(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Layout(content, modifier.fillMaxWidth()) { measurables, constraints ->
        val width = constraints.maxWidth
        val gap = 12.dp.roundToPx()
        if (measurables.isEmpty()) return@Layout layout(width, 0) {}
        val slot = (width - gap * (measurables.size - 1)) / measurables.size
        if (measurables.all { it.maxIntrinsicWidth(Constraints.Infinity) <= slot }) {
            val height = measurables.maxOf { it.minIntrinsicHeight(slot) }
            val placeables = measurables.map { it.measure(Constraints.fixed(slot, height)) }
            layout(width, height) { placeables.forEachIndexed { index, item -> item.place(index * (slot + gap), 0) } }
        } else {
            val placeables = measurables.map { it.measure(Constraints(minWidth = width, maxWidth = width)) }.asReversed()
            layout(width, placeables.sumOf { it.height } + gap * (placeables.size - 1)) {
                var y = 0
                placeables.forEach { item -> item.place(0, y); y += item.height + gap }
            }
        }
    }
}

/** Every modal shares this anatomy: icon tile, title, body, optional content, then the action row. */
@Composable internal fun ZtermDialog(
    onDismissRequest: () -> Unit, title: String, body: String? = null, icon: String? = null,
    busy: Boolean = false, destructive: Boolean = false, dismissOnClickOutside: Boolean = true,
    actions: @Composable () -> Unit, content: (@Composable ColumnScope.() -> Unit)? = null,
) {
    Dialog(onDismissRequest, DialogProperties(dismissOnClickOutside = dismissOnClickOutside, usePlatformDefaultWidth = false)) {
        DialogSurface(Modifier.padding(DialogPadding).imePadding()) {
            Column(Modifier.verticalScroll(rememberScrollState()).padding(DialogPadding)) {
                DialogHeader(title, body, icon, busy, destructive)
                if (content != null) Column(Modifier.padding(top = 16.dp), verticalArrangement = Arrangement.spacedBy(12.dp), content = content)
                DialogActions(Modifier.padding(top = DialogPadding), actions)
            }
        }
    }
}

@Composable internal fun ConfirmDialog(title: String, message: String, action: String,
    dismiss: () -> Unit, icon: String? = null, destructive: Boolean = false, confirm: () -> Unit) {
    BlockStartupUpdatePrompt()
    ZtermDialog(dismiss, title, message, icon, destructive = destructive, actions = {
        DialogButton(stringResource(R.string.cancel), dismiss)
        DialogButton(action, confirm, if (destructive) DialogRole.Destructive else DialogRole.Primary)
    })
}

/**
 * The non-modal variant shown in place of terminal content. Its minimum height keeps the card still while the
 * connection state changes; the actions stay pinned to the bottom edge.
 */
@Composable internal fun WideStatusCard(
    title: String, body: String, modifier: Modifier = Modifier, icon: String = "download",
    busy: Boolean = false, minimumHeight: Dp = 284.dp,
    actions: @Composable ColumnScope.() -> Unit,
) {
    DialogSurface(modifier) {
        Column(Modifier.verticalScroll(rememberScrollState()).heightIn(min = minimumHeight).padding(DialogPadding),
            verticalArrangement = Arrangement.SpaceBetween) {
            Column { DialogHeader(title, body, icon, busy, destructive = false) }
            Column(Modifier.padding(top = DialogPadding), verticalArrangement = Arrangement.spacedBy(12.dp), content = actions)
        }
    }
}
