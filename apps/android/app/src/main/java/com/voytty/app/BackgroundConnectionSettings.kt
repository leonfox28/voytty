package com.voytty.app

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle

@Composable internal fun BackgroundConnectionSettings(state: AppState, repository: AppRepository) {
    val owner = repository.backgroundConnection
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val failed by owner.failed.collectAsStateWithLifecycle()
    var visible by remember { mutableStateOf(owner.visibleNotification()) }
    val settings = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { visible = owner.visibleNotification() }
    DisposableEffect(lifecycle, owner) {
        owner.ensureChannel(context)
        visible = owner.visibleNotification()
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) visible = owner.visibleNotification()
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    SettingsGroup(stringResource(R.string.background_connection)) {
        SettingsRow(stringResource(R.string.keep_background_connection),
            Modifier.testTag("background-connection").toggleable(
                state.saved.preferences.keepBackgroundConnection, enabled = state.initialized, role = Role.Switch,
                onValueChange = { value -> repository.updatePreferences { it.copy(keepBackgroundConnection = value) } }),
            subtitle = stringResource(R.string.background_connection_description)) {
            Switch(state.saved.preferences.keepBackgroundConnection, onCheckedChange = null, enabled = state.initialized)
        }
        GroupDivider()
        SettingsRow(stringResource(R.string.notification_system_settings), Modifier.clickable {
                try { settings.launch(owner.settingsIntent()) }
                catch (_: android.content.ActivityNotFoundException) {
                    android.widget.Toast.makeText(context, R.string.system_settings_unavailable, android.widget.Toast.LENGTH_SHORT).show()
                }
            },
            subtitle = stringResource(if (visible) R.string.background_notification_visible else R.string.background_notification_hidden)) {
            LineIcon("external", Modifier.size(18.dp), MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (failed) Text(stringResource(R.string.background_start_failed), Modifier.padding(start = 16.dp, end = 16.dp, bottom = 16.dp),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
    }
}
