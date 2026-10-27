package io.github.leonfox28.voytty

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

@Composable internal fun DiagnosticsSettings() {
    val app = LocalContext.current.applicationContext as VoyttyApplication
    val diagnostics = app.diagnostics
    val state by diagnostics.state.collectAsStateWithLifecycle()
    var includeDebug by rememberSaveable { mutableStateOf(false) }
    var exportIncludesDebug by rememberSaveable { mutableStateOf(false) }
    val export = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/x-ndjson")) { uri ->
        if (uri != null) diagnostics.export(uri, exportIncludesDebug)
    }
    SettingsGroup(stringResource(R.string.diagnostics)) {
        Text(stringResource(R.string.diagnostics_description), Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        val detailLabel = stringResource(R.string.diagnostics_detail)
        SettingsRow(detailLabel, subtitle = if (state.remainingSeconds > 0)
            stringResource(R.string.diagnostics_remaining, state.remainingSeconds) else null) {
            Switch(state.remainingSeconds > 0, { diagnostics.setEnabled(it) }, enabled = !state.busy, modifier = Modifier.testTag("diagnostics-detail").semantics { contentDescription = detailLabel })
        }
        if (state.remainingSeconds > 0) TextButton({ diagnostics.setEnabled(true) }, Modifier.padding(start = 4.dp), enabled = !state.busy) {
            Text(stringResource(R.string.diagnostics_renew))
        }
        GroupDivider()
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 16.dp, top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Checkbox(includeDebug, { includeDebug = it }, enabled = !state.busy, modifier = Modifier.testTag("diagnostics-include-detail"))
            Text(stringResource(R.string.diagnostics_include_detail), style = MaterialTheme.typography.bodyMedium)
        }
        OutlinedButton({ exportIncludesDebug = includeDebug; export.launch("voytty-logs.jsonl") },
            Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 16.dp).heightIn(min = 44.dp),
            enabled = !state.busy, shape = MaterialTheme.shapes.small) {
            LineIcon("download", Modifier.size(18.dp)); Spacer(Modifier.width(8.dp)); Text(stringResource(R.string.diagnostics_export))
        }
        if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 16.dp))
        if (state.failed) Text(stringResource(R.string.diagnostics_failed), Modifier.padding(start = 16.dp, end = 16.dp, bottom = 16.dp),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
    }
}
