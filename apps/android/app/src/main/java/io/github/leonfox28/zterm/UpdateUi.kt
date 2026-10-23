package io.github.leonfox28.zterm

import android.content.ClipData
import android.content.Intent
import android.provider.Settings
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.core.net.toUri
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle

internal val LocalUpdateBlocks = staticCompositionLocalOf<MutableIntState?> { null }

/** Existing modal owners can defer an unsolicited startup prompt. */
@Composable internal fun BlockStartupUpdatePrompt() {
    val blocks = LocalUpdateBlocks.current
    DisposableEffect(blocks) {
        if (blocks != null) blocks.intValue++
        onDispose { if (blocks != null) blocks.intValue-- }
    }
}

@Composable private fun UpdateDialog(title: String, body: String, dismiss: () -> Unit,
    actions: @Composable () -> Unit, content: (@Composable ColumnScope.() -> Unit)? = null) {
    ZtermDialog(dismiss, title, body, "download", actions = actions, content = content)
}

@Composable internal fun AppUpdateHost(updates: AppUpdates, appState: AppState) {
    val state by updates.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var resumed by remember(lifecycle) { mutableStateOf(lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) }
    val blocks = LocalUpdateBlocks.current?.intValue ?: 0
    val automaticAllowed = appState.route in setOf(Route.Home, Route.Settings) &&
        !appState.busy && appState.error == null && blocks == 0
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, _ -> resumed = lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED) }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(appState.initialized, resumed) {
        if (appState.initialized && resumed) {
            withFrameNanos { }
            updates.startup()
        }
    }
    val message = state.notice?.let { stringResource(updateMessage(it.code)) }
    LaunchedEffect(state.notice?.id, resumed) {
        val notice = state.notice
        if (resumed && notice != null && updates.consumeNotice(notice.id))
            Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
    }
    val install = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        updates.installationReturned()
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        updates.permissionReturned(context.packageManager.canRequestPackageInstalls())
    }
    LaunchedEffect(state.phase, resumed, blocks) {
        if (state.phase == UpdatePhase.Ready && resumed && blocks == 0) {
            if (!context.packageManager.canRequestPackageInstalls()) updates.requireInstallPermission()
            else {
                val file = updates.takeInstallFile() ?: return@LaunchedEffect
                try {
                    val uri = FileProvider.getUriForFile(context, context.packageName + ".updates", file)
                    install.launch(Intent(Intent.ACTION_VIEW).setDataAndType(uri, "application/vnd.android.package-archive")
                        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION).apply { clipData = ClipData.newRawUri("zterm update", uri) })
                } catch (_: RuntimeException) { updates.fail("update_install_failed") }
            }
        }
    }
    if (!resumed) return
    when (state.phase) {
        UpdatePhase.Available -> if (blocks == 0 && (state.manual || automaticAllowed)) {
            UpdateDialog(stringResource(R.string.update_available),
                stringResource(R.string.update_confirm_body, BuildConfig.VERSION_NAME, state.details!!.version), updates::dismiss, actions = {
                    DialogButton(stringResource(R.string.update_later), updates::dismiss)
                    DialogButton(stringResource(R.string.update_download_install), updates::download, DialogRole.Primary)
                })
        }
        UpdatePhase.Downloading -> UpdateDialog(stringResource(R.string.update_downloading),
            stringResource(R.string.update_downloading_body, state.details!!.version), updates::cancel,
            actions = { DialogButton(stringResource(R.string.update_cancel_download), updates::cancel) }) {
            val progress = (state.downloaded.toFloat() / state.details!!.length).coerceIn(0f, 1f)
            LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
            Text(stringResource(R.string.update_percent, (progress * 100).toInt()), style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        UpdatePhase.Permission -> UpdateDialog(stringResource(R.string.update_install_permission),
            stringResource(R.string.update_install_permission_body), updates::cancel, actions = {
                DialogButton(stringResource(R.string.cancel), updates::cancel)
                DialogButton(stringResource(R.string.open_system_settings), {
                    try { permission.launch(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, ("package:" + context.packageName).toUri())) }
                    catch (_: RuntimeException) { updates.fail("update_install_failed") }
                }, DialogRole.Primary)
            })
        else -> Unit
    }
}

internal fun updateMessage(code: String): Int = when (code) {
    "update_latest" -> R.string.update_latest
    "update_invalid" -> R.string.update_invalid
    "update_unsupported" -> R.string.update_unsupported
    "update_rate_limit" -> R.string.update_rate_limit
    "update_storage" -> R.string.update_storage
    "update_install_failed" -> R.string.update_install_failed
    "update_download_failed" -> R.string.update_download_failed
    else -> R.string.update_network
}

@Composable internal fun AboutSettings(updates: AppUpdates, enabled: Boolean) {
    val state by updates.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    SettingsGroup(stringResource(R.string.about)) {
        SettingsRow("GitHub", Modifier.clickable {
                try { context.startActivity(Intent(Intent.ACTION_VIEW, "https://github.com/leonfox28/zterm".toUri())) }
                catch (_: android.content.ActivityNotFoundException) {
                    Toast.makeText(context, R.string.browser_unavailable, Toast.LENGTH_SHORT).show()
                }
            }, subtitle = "leonfox28/zterm",
            leading = {
                Box(Modifier.size(40.dp).clip(MaterialTheme.shapes.small).background(MaterialTheme.colorScheme.surfaceContainerHigh),
                    contentAlignment = Alignment.Center) {
                    Icon(androidx.compose.ui.res.painterResource(R.drawable.ic_github), null, Modifier.size(20.dp))
                }
            }) { LineIcon("external", Modifier.size(18.dp), MaterialTheme.colorScheme.onSurfaceVariant) }
        GroupDivider(72.dp)
        LicensesSettings()
        GroupDivider(72.dp)
        val checking = state.phase == UpdatePhase.Checking && state.manual
        SettingsRow(stringResource(if (checking) R.string.update_checking else R.string.check_updates),
            Modifier.clickable(enabled = enabled && state.phase in setOf(UpdatePhase.Idle, UpdatePhase.Checking) && !checking) { updates.check() },
            subtitle = stringResource(R.string.installed_version, BuildConfig.VERSION_NAME),
            leading = { IconTile("download") }) {
            if (checking) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        }
    }
    Spacer(Modifier.height(24.dp))
}
