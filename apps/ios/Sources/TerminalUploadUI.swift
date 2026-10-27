import SwiftUI
import PhotosUI
import UniformTypeIdentifiers

/// Shared native photo picker; selection does not request broad library access.
struct NativePhotoPicker: UIViewControllerRepresentable {
    let selected: (NSItemProvider?) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(selected: selected) }
    func makeUIViewController(context: Context) -> PHPickerViewController {
        var configuration = PHPickerConfiguration()
        configuration.filter = .images; configuration.selectionLimit = 1
        configuration.preferredAssetRepresentationMode = .current
        let picker = PHPickerViewController(configuration: configuration)
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: PHPickerViewController, context: Context) {}
    final class Coordinator: NSObject, PHPickerViewControllerDelegate {
        let selected: (NSItemProvider?) -> Void
        init(selected: @escaping (NSItemProvider?) -> Void) { self.selected = selected }
        func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
            selected(results.first?.itemProvider)
        }
    }
}

struct NativeFilePicker: UIViewControllerRepresentable {
    let selected: (URL?) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(selected: selected) }
    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.item], asCopy: false)
        picker.allowsMultipleSelection = false
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}
    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        let selected: (URL?) -> Void
        init(selected: @escaping (URL?) -> Void) { self.selected = selected }
        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) { selected(urls.first) }
        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { selected(nil) }
    }
}

struct TerminalUploadPresentation: View {
    @ObservedObject var uploads: TerminalUploads
    let strings: AppStrings
    @Binding var presented: Bool
    @State private var sheet: UploadSheet?
    // Retain the outgoing sheet until its dismissal animation completes. A file
    // picker can clear its binding before delivering the selected URL.
    @State private var presentedSheet: UploadSheet?
    @State private var sheetAppeared = false
    @State private var failure: UploadUI?

    private struct UploadSheet: Identifiable {
        let request: UploadUI
        let picking: Bool
        var id: UUID { request.id }
        func matches(_ other: UploadSheet?) -> Bool {
            other?.id == id && other?.picking == picking
        }
    }

    var body: some View {
        Color.clear
            .sheet(item: Binding(get: { sheet }, set: { _ in }), onDismiss: {
                let previous = presentedSheet
                sheet = nil
                presentedSheet = nil
                sheetAppeared = false
                if let previous, previous.picking {
                    uploads.pickerDidDismiss(previous.id)
                    // UIDocumentPicker may finish dismissing before its delegate
                    // delivers the URL. Wait for that callback instead of opening
                    // the same picker again while phase still says "picking".
                    if uploads.state?.id == previous.id, uploads.state?.phase == "picking" { return }
                }
                synchronize()
            }) { surface in
                Group {
                    if surface.picking {
                        Group {
                            if surface.request.picker == .image {
                                NativePhotoPicker { uploads.selected(surface.id, image: $0) }
                            } else {
                                NativeFilePicker { uploads.selected(surface.id, file: $0) }
                            }
                        }.ignoresSafeArea()
                            .onAppear { uploads.pickerPresented(surface.id) }
                    } else {
                        UploadProgressSheet(uploads: uploads, initial: surface.request, strings: strings)
                            .presentationDetents([.medium, .large])
                    }
                }
                // Native Cancel is explicit. An unreported swipe must not leave
                // a picker reservation or upload running without its UI.
                .interactiveDismissDisabled()
                .onAppear { sheetAppeared = true; synchronize() }
            }
            .alert(strings("上传失败"), isPresented: Binding(get: { failure != nil }, set: {
                if !$0 { failure = nil; presented = presentedSheet != nil }
            }), presenting: failure) { _ in
                Button(strings("重试")) { uploads.retry() }
                    .accessibilityIdentifier("retryUpload")
                Button(strings("关闭"), role: .cancel) { uploads.cancel() }
                    .accessibilityIdentifier("closeUploadError")
            } message: { request in
                Text(uploadMessage(request.error ?? "upload_source_invalid", strings: strings))
            }
            .onAppear { synchronize() }
            .onChange(of: uploads.state?.id) { _, _ in synchronize() }
            .onChange(of: uploads.state?.phase) { _, _ in synchronize() }
    }

    private func synchronize() {
        let desired: UploadSheet?
        if let request = uploads.state, request.phase == "picking" || (request.active && request.showProgress) {
            desired = UploadSheet(request: request, picking: request.phase == "picking")
        } else { desired = nil }

        if let current = presentedSheet {
            // A fast failure can arrive before the progress sheet first appears.
            // Keep it admitted until onAppear, so onDismiss can release ownership.
            if sheetAppeared && !current.matches(desired) { sheet = nil }
            return // Present the next surface only from the sheet's onDismiss.
        }
        if failure != nil {
            if uploads.state?.id == failure?.id, uploads.state?.phase == "failed" { return }
            failure = nil
        }
        if let desired {
            presentedSheet = desired
            sheet = desired
        } else if uploads.state?.phase == "failed" {
            failure = uploads.state
        }
        presented = presentedSheet != nil || failure != nil
    }
}

private struct UploadProgressSheet: View {
    @ObservedObject var uploads: TerminalUploads
    let initial: UploadUI
    let strings: AppStrings
    private var upload: UploadUI {
        if let current = uploads.state, current.id == initial.id { return current }
        return initial
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Label(upload.filename.isEmpty ? strings("所选文件") : upload.filename,
                          systemImage: upload.picker == .image ? "photo" : "paperclip")
                    Text(upload.totalBytes.map { strings("大小：%@ MB", decimal(Double($0) / 1_000_000)) }
                         ?? strings("尚未确定文件大小")).foregroundStyle(.secondary)
                }
                Section {
                    if upload.phase == "preparing" {
                        ProgressView().progressViewStyle(.linear)
                        Text(strings("正在准备文件…")).foregroundStyle(.secondary)
                    } else {
                        ProgressView(value: upload.fraction)
                        HStack {
                            Text("\(Int(upload.fraction * 100))%")
                            Spacer()
                            Text(strings("速度：%@ MB/s", decimal(upload.bytesPerSecond / 1_000_000)))
                        }.foregroundStyle(.secondary)
                        if ["finishing", "inserting"].contains(upload.phase) {
                            Text(strings("正在完成上传并插入路径…")).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .accessibilityIdentifier("uploadProgress")
            .navigationTitle(strings("上传")).navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(strings("取消")) { uploads.cancel() }.accessibilityIdentifier("cancelUpload")
                }
            }
        }
    }

    private func decimal(_ value: Double) -> String { String(format: "%.2f", locale: strings.locale, value) }
}

func uploadMessage(_ code: String, strings: AppStrings) -> String {
    switch code {
    case "upload_background_expired": return strings("后台时间已用完。请保持应用在前台后重试；文件可能已上传，但此次结果未确认。")
    case "upload_too_large": return strings("请选择不超过 50 MB 的单个文件。")
    case "upload_source_invalid", "upload_already_started": return strings("无法读取此文件，请重新选择。")
    case "upload_storage_failed": return strings("远端主机无法保存文件。")
    case "upload_outcome_unknown": return strings("文件可能已上传，但结果未确认。重试会创建另一个文件。")
    case "service_not_implemented": return strings("请升级远端主机的 voytty 以使用文件上传。")
    case "input_not_ready", "lease_lost": return strings("等待连接恢复，此次输入未发送。")
    default: return strings("操作未完成（%@）。请检查电脑和网络后重试。", code)
    }
}
