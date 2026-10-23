import SwiftUI
import UserNotifications

struct NotificationSettings: View {
    @ObservedObject var model: AppModel
    @ObservedObject var notifications: TerminalNotifications
    private var strings: AppStrings { model.strings }
    var body: some View {
        Section {
            Toggle(strings("允许终端通知"), isOn: Binding(get: { notifications.enabled }, set: { enabled in
                Task { await model.setNotificationsEnabled(enabled) }
            })).disabled(!model.ready || notifications.busy).accessibilityIdentifier("terminalNotifications")
            if notifications.permission == .denied {
                Text(strings("通知已在系统设置中关闭。"))
                    .font(.footnote).foregroundStyle(AppTheme.secondary)
                Button(strings("打开通知设置")) {
                    if let url = URL(string: UIApplication.openNotificationSettingsURLString) { UIApplication.shared.open(url) }
                }.accessibilityIdentifier("notificationSystemSettings")
            }
            if notifications.failed { Text(strings("无法请求通知权限，请稍后重试。")) .font(.footnote).foregroundStyle(AppTheme.error) }
        } header: { Text(strings("终端通知")) } footer: {
            Text(strings("接收当前会话中程序发送的通知。iOS 暂停后台连接后无法接收，返回应用不会补发。"))
        }.task { await notifications.refresh() }
    }
}

struct DiagnosticsSettings: View {
    @ObservedObject var diagnostics: AppDiagnostics
    let strings: AppStrings

    var body: some View {
        Section {
            Toggle(strings("开启诊断（15 分钟）"), isOn: Binding(get: { diagnostics.remainingSeconds > 0 }, set: { value in
                Task { await diagnostics.setEnabled(value) }
            })).disabled(diagnostics.busy).accessibilityIdentifier("diagnosticsDetail")
            if diagnostics.remainingSeconds > 0 {
                HStack {
                    Text(strings("剩余 %@ 秒", String(diagnostics.remainingSeconds))).font(.footnote)
                    Spacer()
                    Button(strings("续期")) { Task { await diagnostics.setEnabled(true) } }.disabled(diagnostics.busy)
                }.foregroundStyle(AppTheme.secondary)
            }
            Button {
                Task { await diagnostics.prepareExport() }
            } label: {
                HStack {
                    Text(strings("导出日志"))
                    Spacer()
                    if diagnostics.busy { ProgressView() }
                    else { Image(systemName: "square.and.arrow.up").accessibilityHidden(true) }
                }.contentShape(Rectangle())
            }.disabled(diagnostics.busy).accessibilityIdentifier("diagnosticsExport")
            if diagnostics.failed {
                Text(strings("无法保存或导出诊断日志，请稍后重试。"))
                    .font(.footnote).foregroundStyle(AppTheme.error)
            }
        } header: { Text(strings("诊断")) }
    }
}

/// Keep ticking and presentation outside Form's lazily reused Section rows.
struct DiagnosticExportPresentation: View {
    @ObservedObject var diagnostics: AppDiagnostics
    @State private var presentedExport: UUID?

    var body: some View {
        Color.clear
        .task { await diagnostics.observe() }
        // UIKit can clear the binding before delivering its selected URL. Its
        // delegate owns completion; each callback belongs to one export only.
        .sheet(item: Binding(get: { diagnostics.export }, set: { _ in }), onDismiss: {
            if let id = presentedExport { diagnostics.exportDismissed(id: id) }
            presentedExport = nil
        }) { export in
            DiagnosticExportPicker(url: export.url) { diagnostics.finishExport(id: export.id, saved: $0) }
                // A completed UIDocumentPicker cannot be reused for another
                // export, even when its new staging URL has the same name.
                .id(export.id)
                .ignoresSafeArea().interactiveDismissDisabled()
                .onAppear { presentedExport = export.id }
        }
    }
}

/// UIKit owns filename editing, destination selection, overwrite confirmation and cancellation.
struct DiagnosticExportPicker: UIViewControllerRepresentable {
    let url: URL
    let completed: (Bool) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(completed: completed) }
    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let controller = UIDocumentPickerViewController(forExporting: [url], asCopy: true)
        controller.delegate = context.coordinator
        return controller
    }
    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}
    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        let completed: (Bool) -> Void
        init(completed: @escaping (Bool) -> Void) { self.completed = completed }
        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) { completed(!urls.isEmpty) }
        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { completed(false) }
    }
}
