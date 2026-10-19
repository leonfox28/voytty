package io.github.leonfox28.zterm

import android.Manifest
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

@Composable internal fun ScannerScreen(state: AppState, repository: AppRepository) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val active = remember { AtomicBoolean(true) }
    DisposableEffect(Unit) { onDispose { active.set(false) } }
    var manual by remember { mutableStateOf(false) }
    var choosing by remember { mutableStateOf(false) }
    var imageBusy by remember { mutableStateOf(false) }
    var localError by remember { mutableStateOf<String?>(null) }
    var candidates by remember { mutableStateOf<List<Pair<String, io.github.leonfox28.zterm.nativebridge.NativeHost>>>(emptyList()) }
    val activity = androidx.activity.compose.LocalActivity.current
    var permanentlyDenied by remember { mutableStateOf(false) }
    var permitted by remember { mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        permitted = it
        permanentlyDenied = !it && activity?.shouldShowRequestPermissionRationale(Manifest.permission.CAMERA) == false
    }
    val lifecycle = LocalLifecycleOwner.current
    DisposableEffect(lifecycle) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            if (event == androidx.lifecycle.Lifecycle.Event.ON_RESUME) permitted = ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
        }
        lifecycle.lifecycle.addObserver(observer)
        onDispose { lifecycle.lifecycle.removeObserver(observer) }
    }
    val codeReady: (List<String>) -> Unit = { codes ->
        if (active.get()) {
            val valid = codes.distinct().mapNotNull { ticket ->
                try { ticket to repository.runtime.inspectTicket(ticket) } catch (_: Exception) { null }
            }
            when (valid.size) {
                0 -> localError = if (codes.isEmpty()) "no_qr" else "invalid_ticket"
                1 -> repository.pair(valid.single().first)
                else -> candidates = valid
            }
        }
    }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        choosing = false
        if (uri != null) {
            imageBusy = true
            scope.launch {
                try {
                    val bitmap = withContext(Dispatchers.IO) { readQrBitmap(context, uri) }
                    val scanner = BarcodeScanning.getClient(BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build())
                    scanner.process(InputImage.fromBitmap(bitmap, 0))
                        .addOnSuccessListener { values -> codeReady(values.mapNotNull { it.rawValue }.distinct()) }
                        .addOnFailureListener { if (active.get()) localError = "image_unreadable" }
                        .addOnCompleteListener { bitmap.recycle(); scanner.close(); imageBusy = false }
                } catch (_: Exception) { localError = "image_unreadable"; imageBusy = false }
            }
        }
    }
    LaunchedEffect(Unit) { if (!permitted) permission.launch(Manifest.permission.CAMERA) }
    BackHandler { if (manual) manual = false else if (candidates.isNotEmpty()) candidates = emptyList() else repository.show(Route.Home) }
    val paused = manual || choosing || imageBusy || candidates.isNotEmpty() || state.busy || state.error != null || localError != null
    Column(Modifier.fillMaxSize().safeDrawingPadding()) {
        PageHeader(stringResource(R.string.scan)) { repository.show(Route.Home) }
        Box(Modifier.weight(1f).fillMaxWidth().padding(horizontal = 16.dp), contentAlignment = Alignment.Center) {
            Surface(Modifier.fillMaxSize(), color = Color(0xFF0B0E0C), shape = MaterialTheme.shapes.extraLarge) {
                if (permitted && !paused) CameraPreview(codeReady) { localError = "camera_unavailable" }
            }
            if (!permitted) Column(horizontalAlignment = Alignment.CenterHorizontally) {
                LineIcon("image", Modifier.padding(bottom = 16.dp).size(40.dp), Color.White.copy(alpha = .6f))
                Text(stringResource(R.string.camera_denied), color = Color.White, style = MaterialTheme.typography.titleMedium)
                Button({
                    if (permanentlyDenied) context.startActivity(android.content.Intent(android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${context.packageName}")))
                    else permission.launch(Manifest.permission.CAMERA)
                }, Modifier.padding(top = 16.dp)) { Text(stringResource(if (permanentlyDenied) R.string.open_settings else R.string.allow_camera)) }
            }
            if (permitted && !paused) {
                ScanCorners()
                Surface(Modifier.align(Alignment.TopCenter).padding(top = 20.dp, start = 20.dp, end = 20.dp), shape = CircleShape,
                    color = Color.Black.copy(alpha = .55f), contentColor = Color.White) {
                    Text(stringResource(R.string.scan_hint), Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                        style = MaterialTheme.typography.bodyMedium)
                }
            }
            if (state.busy || imageBusy) CircularProgressIndicator()
            val failure = localError ?: state.error
            if (!manual && failure != null) DialogSurface(Modifier.align(Alignment.BottomCenter).padding(16.dp)) {
                Column(Modifier.padding(20.dp)) {
                    Text(errorMessage(failure), style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.error)
                    DialogActions(Modifier.padding(top = 16.dp)) {
                        DialogButton(stringResource(R.string.retry), { localError = null; repository.clearError() }, DialogRole.Primary)
                    }
                }
            }
        }
        Row(Modifier.fillMaxWidth().padding(16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            FilledTonalButton({ localError = null; repository.clearError(); choosing = true; picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
                Modifier.weight(1f).height(52.dp), enabled = !state.busy && !imageBusy, shape = MaterialTheme.shapes.medium) {
                LineIcon("image", Modifier.size(20.dp)); Spacer(Modifier.width(8.dp)); Text(stringResource(R.string.choose_image), maxLines = 1)
            }
            FilledTonalButton({ localError = null; repository.clearError(); manual = true }, Modifier.weight(1f).height(52.dp),
                enabled = !state.busy && !imageBusy, shape = MaterialTheme.shapes.medium) {
                LineIcon("keyboard", Modifier.size(20.dp)); Spacer(Modifier.width(8.dp)); Text(stringResource(R.string.enter_credentials), maxLines = 1)
            }
        }
    }
    if (manual) CredentialDialog(state.busy, state.error, repository.runtime.ticketTextLimit().toInt(), { manual = false; repository.clearError() }, repository::pair)
    if (candidates.isNotEmpty()) ZtermDialog({ candidates = emptyList() }, stringResource(R.string.choose_device), icon = "server",
        actions = { DialogButton(stringResource(R.string.cancel), { candidates = emptyList() }) }) {
        candidates.forEach { (ticket, host) ->
            Surface({ candidates = emptyList(); repository.pair(ticket) }, Modifier.fillMaxWidth(), shape = MaterialTheme.shapes.medium,
                color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                    Text(host.name, style = MaterialTheme.typography.bodyLarge)
                    Text(host.deviceId.takeLast(8), style = MaterialTheme.typography.bodySmall, fontFamily = TerminalFont.family,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
}
@Composable private fun CredentialDialog(busy: Boolean, error: String?, limit: Int, dismiss: () -> Unit, connect: (String) -> Unit) {
    // A bearer is transient composition state, never saved into Activity bundles.
    var ticket by remember { mutableStateOf("") }
    ZtermDialog(dismiss, stringResource(R.string.enter_credentials), icon = "keyboard", actions = {
        DialogButton(stringResource(R.string.cancel), dismiss)
        DialogButton(stringResource(R.string.connect), { connect(ticket) }, DialogRole.Primary, enabled = ticket.isNotBlank() && !busy, busy = busy)
    }) {
        OutlinedTextField(ticket, { if (it.toByteArray().size <= limit) ticket = it }, Modifier.fillMaxWidth().heightIn(min = 116.dp, max = 180.dp),
            enabled = !busy, placeholder = { Text(stringResource(R.string.credential_hint)) }, isError = error != null, shape = MaterialTheme.shapes.medium)
        ErrorText(error)
    }
}
@Composable private fun ScanCorners() {
    val tint = Color.White
    Canvas(Modifier.fillMaxWidth(.72f).aspectRatio(1f)) {
        val width = size.width; val height = size.height; val arm = width * .12f
        for ((x,dx) in listOf(0f to 1f, width to -1f)) for ((y,dy) in listOf(0f to 1f, height to -1f)) {
            drawLine(tint, androidx.compose.ui.geometry.Offset(x,y), androidx.compose.ui.geometry.Offset(x+dx*arm,y), 4.dp.toPx(), androidx.compose.ui.graphics.StrokeCap.Round)
            drawLine(tint, androidx.compose.ui.geometry.Offset(x,y), androidx.compose.ui.geometry.Offset(x,y+dy*arm), 4.dp.toPx(), androidx.compose.ui.graphics.StrokeCap.Round)
        }
    }
}
@androidx.annotation.OptIn(androidx.camera.core.ExperimentalGetImage::class)
@Composable private fun CameraPreview(onCodes: (List<String>) -> Unit, onError: () -> Unit) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current
    val previewView = remember { PreviewView(context).apply { scaleType = PreviewView.ScaleType.FILL_CENTER } }
    val latestCodes by rememberUpdatedState(onCodes)
    val latestError by rememberUpdatedState(onError)
    AndroidView(factory = { previewView }, modifier = Modifier.fillMaxSize())
    DisposableEffect(lifecycle, previewView) {
        val retired = AtomicBoolean(false)
        val processing = AtomicBoolean(false)
        val recognized = AtomicBoolean(false)
        val executor = Executors.newSingleThreadExecutor()
        val scanner = BarcodeScanning.getClient(BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build())
        val future = ProcessCameraProvider.getInstance(context)
        var provider: ProcessCameraProvider? = null
        val preview = Preview.Builder().build().also { it.surfaceProvider = previewView.surfaceProvider }
        val analysis = ImageAnalysis.Builder().setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST).build()
        analysis.setAnalyzer(executor) { proxy ->
            val media = proxy.image
            if (retired.get() || recognized.get() || media == null || !processing.compareAndSet(false,true)) { proxy.close() }
            else scanner.process(InputImage.fromMediaImage(media, proxy.imageInfo.rotationDegrees))
                .addOnSuccessListener { codes ->
                    val values = codes.mapNotNull { it.rawValue }.distinct()
                    if (!retired.get() && values.isNotEmpty() && recognized.compareAndSet(false,true)) latestCodes(values)
                }
                .addOnFailureListener { if (!retired.get()) latestError() }
                .addOnCompleteListener { proxy.close(); processing.set(false) }
        }
        future.addListener({
            if (!retired.get()) try { provider = future.get(); provider?.bindToLifecycle(lifecycle, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis) }
            catch (_: Exception) { latestError() }
        }, ContextCompat.getMainExecutor(context))
        onDispose {
            retired.set(true); analysis.clearAnalyzer(); provider?.unbind(preview, analysis)
            executor.shutdown(); scanner.close()
        }
    }
}
private fun readQrBitmap(context: android.content.Context, uri: Uri): Bitmap {
    if (Build.VERSION.SDK_INT >= 28) return ImageDecoder.decodeBitmap(ImageDecoder.createSource(context.contentResolver, uri)) { decoder, info, _ ->
        val ratio = maxOf(info.size.width, info.size.height).toFloat() / 2048
        if (ratio > 1) decoder.setTargetSize((info.size.width / ratio).toInt().coerceAtLeast(1), (info.size.height / ratio).toInt().coerceAtLeast(1))
        decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
    }
    val options = android.graphics.BitmapFactory.Options().apply { inJustDecodeBounds = true }
    context.contentResolver.openInputStream(uri).use { android.graphics.BitmapFactory.decodeStream(it, null, options) }
    if (options.outWidth <= 0 || options.outHeight <= 0) throw java.io.IOException()
    options.inSampleSize = 1
    while (maxOf(options.outWidth, options.outHeight) / options.inSampleSize > 2048) options.inSampleSize *= 2
    options.inJustDecodeBounds = false
    return context.contentResolver.openInputStream(uri).use { android.graphics.BitmapFactory.decodeStream(it, null, options) } ?: throw java.io.IOException()
}
