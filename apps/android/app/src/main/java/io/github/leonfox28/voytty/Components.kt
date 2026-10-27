package io.github.leonfox28.voytty

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

@Composable internal fun PageHeader(title: String, back: () -> Unit) {
    Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        IconAction("back", stringResource(R.string.back), onClick = back)
        Text(title, Modifier.padding(start = 4.dp), style = MaterialTheme.typography.titleLarge)
    }
}

/** Small uppercase-style label above a grouped card. */
@Composable internal fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(text, modifier.padding(start = 16.dp, top = 24.dp, bottom = 8.dp),
        style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
}

/** One rounded container holding related rows separated by inset dividers. */
@Composable internal fun SettingsGroup(title: String? = null, content: @Composable ColumnScope.() -> Unit) {
    if (title != null) SectionLabel(title)
    Surface(Modifier.fillMaxWidth(), shape = MaterialTheme.shapes.large, color = MaterialTheme.colorScheme.surfaceContainer) {
        Column(content = content)
    }
}

@Composable internal fun GroupDivider(start: Dp = 16.dp) {
    HorizontalDivider(Modifier.padding(start = start, end = 16.dp), color = MaterialTheme.colorScheme.outlineVariant)
}

/** A rounded square holding a line icon; used as the leading visual of rows and cards. */
@Composable internal fun IconTile(icon: String, modifier: Modifier = Modifier, size: Dp = 40.dp,
    container: Color = MaterialTheme.colorScheme.surfaceContainerHigh, tint: Color = MaterialTheme.colorScheme.onSurfaceVariant) {
    Box(modifier.size(size).clip(MaterialTheme.shapes.small).background(container), contentAlignment = Alignment.Center) {
        LineIcon(icon, Modifier.size(size * .5f), tint)
    }
}

/** Title, optional supporting line and trailing content, laid out as a settings row. */
@Composable internal fun SettingsRow(title: String, modifier: Modifier = Modifier, subtitle: String? = null,
    leading: (@Composable () -> Unit)? = null, trailing: (@Composable () -> Unit)? = null) {
    Row(modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically) {
        if (leading != null) { leading(); Spacer(Modifier.width(16.dp)) }
        Column(Modifier.weight(1f).padding(end = 12.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            if (subtitle != null) Text(subtitle, Modifier.padding(top = 2.dp), style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 3, overflow = TextOverflow.Ellipsis)
        }
        trailing?.invoke()
    }
}

@Composable internal fun StatusDot(color: Color, modifier: Modifier = Modifier) {
    Box(modifier.size(8.dp).clip(CircleShape).background(color))
}
