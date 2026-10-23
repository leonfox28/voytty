package io.github.leonfox28.zterm

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp

@Composable internal fun LicensesSettings() {
    val context = LocalContext.current
    var open by remember { mutableStateOf(false) }
    var selected by remember { mutableStateOf<String?>(null) }
    SettingsRow(stringResource(R.string.licenses), Modifier.clickable { open = true },
        leading = { IconTile("terminal") }) { LineIcon("right") }
    if (!open) return
    BlockStartupUpdatePrompt()
    val files = remember(context) { context.assets.list("licenses")?.sorted().orEmpty() }
    val file = selected
    val contents = remember(context, file) {
        file?.let { name ->
            runCatching { context.assets.open("licenses/$name").bufferedReader().use { it.readText() } }.getOrNull()
        }
    }
    AlertDialog(onDismissRequest = { if (selected != null) selected = null else open = false },
        title = { Text(file ?: stringResource(R.string.licenses)) },
        text = {
            Column(Modifier.fillMaxWidth().heightIn(max = 440.dp).verticalScroll(rememberScrollState())) {
                if (file == null) {
                    files.forEach { name ->
                        Text(name, Modifier.fillMaxWidth().clickable { selected = name }.padding(vertical = 12.dp))
                    }
                } else {
                    SelectionContainer {
                        Text(contents ?: stringResource(R.string.license_unavailable), style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { if (selected != null) selected = null else open = false }) {
                Text(stringResource(if (selected != null) R.string.license_back else R.string.close))
            }
        })
}
