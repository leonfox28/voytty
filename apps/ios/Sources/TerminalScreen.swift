import SwiftUI

private struct SessionEdit: Identifiable {
    let session: NativeSession?
    var id: String { session?.sessionId ?? "new" }
}

struct TerminalScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject private var uploads: TerminalUploads
    private var strings: AppStrings { model.strings }
    init(model: AppModel) { self.model = model; uploads = model.uploads }
    @State private var keyboard = false
    @State private var keyboardBeforeUpload = false
    @State private var uploadPresented = false
    @State private var editor: SessionEdit?
    @State private var editorPresented = false
    @State private var panelPresented = false
    @State private var keyboardBeforePanel = false
    @State private var deleting: NativeSession?
    @State private var taking: NativeSession?

    private var status: String {
        switch model.frame?.state {
        case "active":
            let route = model.frame?.connectionPath == .direct ? "Direct" : model.frame?.connectionPath == .relay ? "Relay" : strings("连接中")
            return "\(route) · \(model.frame?.rttMs.map(String.init) ?? "—") ms"
        case "synchronizing": return strings("正在同步…")
        case "reconnecting": return strings("重连中…")
        case "lease_lost": return strings("已在其他设备连接")
        case "ended": return strings("会话已结束")
        default: return model.connecting ? strings("连接中…") : strings("已断开")
        }
    }
    private var statusColor: Color {
        model.frame?.state == "active" ? AppTheme.success : model.connecting || ["reconnecting", "synchronizing"].contains(model.frame?.state ?? "") ? AppTheme.warning : AppTheme.secondary
    }
    private var inactive: Bool {
        model.frame == nil || ["ended", "closed", "lease_lost"].contains(model.frame?.state ?? "")
    }
    private var connectionNotice: ConnectionNotice? {
        guard !model.busy, !model.panel, !panelPresented, !editorPresented, deleting == nil, taking == nil,
              uploads.state == nil, !uploads.paused, !uploadPresented else { return nil }
        return model.connectionNotice
    }

    private func noticePresented(_ notice: ConnectionNotice?) -> Binding<Bool> {
        Binding(get: { notice != nil }, set: { presented in
            if !presented, let notice { model.dismissConnectionNotice(notice.id) }
        })
    }

    private var panelPresentation: Binding<Bool> {
        // A successful create starts attaching before its form finishes. Keep
        // the presenting sheet alive until the child form has fully dismissed.
        Binding(get: { model.panel || editorPresented }, set: { presented in
            if !presented { model.panel = false }
        })
    }

    private func openPanel() {
        keyboardBeforePanel = keyboard
        keyboard = false
        model.togglePanel()
    }

    var body: some View {
        VStack(spacing: 0) {
            ZStack(alignment: .bottom) {
                TerminalSurface(model: model, keyboard: $keyboard,
                                acceptsKeyboard: !inactive && !model.panel && !panelPresented && !editorPresented && deleting == nil
                                    && taking == nil && model.connectionNotice == nil
                                    && uploads.state == nil && !uploads.paused && !uploadPresented)
                if model.frame == nil && !model.panel { connectionStatus.frame(maxWidth: .infinity, maxHeight: .infinity) }
                VStack {
                    if ["reconnecting", "synchronizing"].contains(model.frame?.state ?? "") {
                        ProgressView().progressViewStyle(.linear)
                            .accessibilityIdentifier("connectionProgress")
                    }
                    if let error = model.error, !inactive, !model.panel {
                        ErrorBanner(text: error, dismiss: { model.error = nil }).padding(8)
                    }
                    Spacer()
                }.allowsHitTesting(model.error != nil)
            }
            Rectangle().fill(AppTheme.outline).frame(height: 1)
            toolbar
        }
        .navigationTitle(model.selectedHost?.name ?? "zterm")
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    Text(model.selectedHost?.name ?? "zterm")
                        .font(.headline).lineLimit(1).truncationMode(.middle)
                    HStack(spacing: 5) {
                        Circle().fill(statusColor).frame(width: 6, height: 6).accessibilityHidden(true)
                        Text(status).font(.caption).monospacedDigit().lineLimit(1)
                            .foregroundStyle(AppTheme.secondary)
                    }
                }.accessibilityElement(children: .combine)
                    .accessibilityIdentifier("terminalConnectionStatus")
            }
        }
        .background(AppTheme.background)
        .background { TerminalUploadPresentation(uploads: uploads, strings: strings, presented: $uploadPresented) }
        .onChange(of: uploads.state?.id) { _, current in
            if current == nil { keyboard = keyboardBeforeUpload }
        }
        .sheet(isPresented: panelPresentation, onDismiss: {
            panelPresented = false
            keyboard = keyboardBeforePanel
            keyboardBeforePanel = false
        }) {
            sessionPanel
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
                .interactiveDismissDisabled(model.busy || editorPresented || deleting != nil || taking != nil)
                .onAppear { panelPresented = true }
        }
        .alert(strings(connectionNotice?.title ?? "连接失败"), isPresented: noticePresented(connectionNotice),
               presenting: connectionNotice) { notice in
            if notice.takeover, let session = notice.session {
                Button(strings("确认接管"), role: .destructive) {
                    guard model.connectionNoticeIsCurrent(notice) else { return }
                    model.dismissConnectionNotice(notice.id)
                    Task { await model.connect(id: session, takeover: true) }
                }.accessibilityIdentifier("confirmTakeoverSession")
            } else if notice.kind == .failed {
                Button(strings("重试")) {
                    guard model.connectionNoticeIsCurrent(notice) else { return }
                    model.dismissConnectionNotice(notice.id)
                    Task { await model.retry() }
                }.accessibilityIdentifier("retryConnectionAlert")
            }
            Button(strings("会话列表")) {
                guard model.connectionNoticeIsCurrent(notice) else { return }
                model.dismissConnectionNotice(notice.id)
                model.panel = true
                Task { await model.refresh() }
            }.accessibilityIdentifier("connectionAlertSessions")
            Button(strings("取消"), role: .cancel) { model.dismissConnectionNotice(notice.id) }
                .accessibilityIdentifier("dismissConnectionNotice")
        } message: { notice in
            if notice.takeover {
                Text(strings("此会话正由另一台设备控制。确认接管后，另一台设备将失去控制权，远端进程继续运行。"))
            } else if notice.kind == .ended {
                Text(strings("会话已结束，请选择其他会话。"))
            } else { Text(notice.message) }
        }
    }

    private var toolbar: some View {
        HStack(spacing: 3) {
            Button { openPanel() } label: {
                HStack(spacing: 5) {
                    Circle().fill(statusColor).frame(width: 6, height: 6)
                    Text(model.sessionName.isEmpty ? model.selectedHost?.name ?? "zterm" : model.sessionName)
                        .font(.system(size: 12, weight: .medium)).lineLimit(1)
                    Image(systemName: model.panel ? "chevron.down" : "chevron.up").font(.system(size: 9, weight: .semibold))
                }.padding(.horizontal, 8).frame(height: 36)
                    .background(model.panel ? AppTheme.primaryContainer : AppTheme.elevated, in: RoundedRectangle(cornerRadius: 10))
            }.frame(minWidth: 58, maxWidth: 88).accessibilityLabel(strings("会话列表")).accessibilityIdentifier("sessionChip")
            .contextMenu {
                Button(strings("粘贴"), systemImage: "doc.on.clipboard") {
                    if let text = UIPasteboard.general.string { model.text(text, paste: true) }
                }
                Button("Tab") { model.key(.tab) }
                Button(model.alt ? strings("取消 Alt") : "Alt") { model.alt.toggle() }
            }.accessibilityHint(strings("长按显示终端操作"))
            uploadButton(.image, icon: "photo", label: "上传图片")
            uploadButton(.file, icon: "paperclip", label: "上传文件")
            key("Esc", .escape)
            Button { model.control.toggle() } label: {
                Text("Ctrl").font(.system(size: 13, weight: .medium)).frame(maxWidth: .infinity).frame(height: 40)
                    .background(model.control ? AppTheme.primaryContainer : .clear, in: RoundedRectangle(cornerRadius: 8))
            }.disabled(!model.inputReady)
            arrow("arrow.left", .left, strings("向左"))
            arrow("arrow.down", .down, strings("向下"))
            arrow("arrow.up", .up, strings("向上"))
            arrow("arrow.right", .right, strings("向右"))
            Button { keyboard.toggle() } label: {
                Image(systemName: "keyboard").font(.system(size: 17)).frame(maxWidth: .infinity).frame(height: 40)
                    .background(keyboard ? AppTheme.primaryContainer : .clear, in: RoundedRectangle(cornerRadius: 8))
            }.disabled(!model.inputReady && !keyboard)
                .accessibilityLabel(keyboard ? strings("收起键盘") : strings("展开键盘")).accessibilityIdentifier("keyboardToggle")
        }.buttonStyle(.plain).padding(.horizontal, 4).frame(height: 52).background(AppTheme.chrome)
    }

    private func uploadButton(_ picker: UploadPicker, icon: String, label: String) -> some View {
        Button {
            model.panel = false
            keyboardBeforeUpload = keyboard
            keyboard = false
            uploads.choose(picker)
        } label: {
            Image(systemName: icon).font(.system(size: 17)).frame(maxWidth: .infinity).frame(height: 40)
        }.disabled(!model.inputReady).accessibilityLabel(strings(label))
            .accessibilityIdentifier(picker == .image ? "uploadImage" : "uploadFile")
    }

    private var sessionPanel: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(model.sessions, id: \.sessionId) { session in sessionRow(session) }
                    if model.sessions.isEmpty && !model.sessionsRefreshing {
                        Text(strings("暂无会话")).foregroundStyle(.secondary)
                    }
                }
                if let error = model.error { Text(error).foregroundStyle(AppTheme.error) }
                if let error = model.sessionListError {
                    Section {
                        Text(strings("无法刷新会话列表：%@", error)).foregroundStyle(AppTheme.error)
                        Button(strings("重试")) { Task { await model.refresh() } }.disabled(model.sessionsRefreshing)
                    }
                }
            }.listStyle(.insetGrouped).accessibilityIdentifier("sessionList")
                .refreshable { await model.refresh() }
                .navigationTitle(strings("会话列表")).navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button(strings("完成")) { model.panel = false }
                            .disabled(model.busy).accessibilityIdentifier("closeSessionPanel")
                    }
                    ToolbarItem(placement: .primaryAction) {
                        Button(strings("新建会话"), systemImage: "plus") {
                            editorPresented = true; editor = SessionEdit(session: nil)
                        }.disabled(model.busy).accessibilityIdentifier("newSession")
                    }
                }
        }
        .sheet(item: $editor, onDismiss: { editorPresented = false }) { target in
            SessionEditor(model: model, session: target.session)
        }
        .alert(strings("接管会话“%@”？", taking?.name ?? ""), isPresented: Binding(
            get: { taking != nil }, set: { if !$0 { taking = nil } }), presenting: taking) { session in
            Button(strings("取消"), role: .cancel) { taking = nil }
                .accessibilityIdentifier("cancelTakeoverSession")
            Button(strings("确认接管"), role: .destructive) { Task { await model.connect(session, takeover: true) } }
                .accessibilityIdentifier("confirmTakeoverSession")
        } message: { _ in Text(strings("另一台设备将失去控制权，远端进程继续运行。")) }
    }

    private func sessionRow(_ session: NativeSession) -> some View {
        let current = model.ownsSession && session.sessionId == model.sessionID
        let applicationTitle = model.frame?.applicationTitle.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return HStack(spacing: 0) {
            Button {
                if current { model.panel = false }
                else if session.occupied { taking = session }
                else { Task { await model.connect(session) } }
            } label: {
                HStack(spacing: 12) {
                    Image(systemName: "terminal").font(.system(size: 18))
                        .frame(width: 36, height: 36)
                        .foregroundStyle(current ? AppTheme.onPrimary : AppTheme.secondary)
                        .background(current ? AppTheme.primary : AppTheme.elevated, in: RoundedRectangle(cornerRadius: 10))
                    VStack(alignment: .leading, spacing: 3) {
                        Text(session.name).font(.body.weight(current ? .semibold : .regular)).lineLimit(1)
                        if current || session.occupied {
                            Text(current ? (applicationTitle.isEmpty || applicationTitle == session.name ? strings("当前") : strings("当前 · %@", applicationTitle)) : strings("使用中"))
                                .font(.caption).lineLimit(1)
                                .foregroundStyle(current ? AppTheme.onPrimaryContainer.opacity(0.72) : AppTheme.warning)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }.padding(.vertical, 4)
                    .contentShape(Rectangle())
            }.buttonStyle(.plain).disabled(model.busy).accessibilityIdentifier("session-\(session.sessionId)")
            Menu {
                Button(strings("重命名"), systemImage: "pencil") { editorPresented = true; editor = SessionEdit(session: session) }
                Button(strings("删除"), systemImage: "trash", role: .destructive) { deleting = session }
            } label: { Image(systemName: "ellipsis").rotationEffect(.degrees(90)).frame(width: 44, height: 48) }
                .buttonStyle(.borderless).disabled(model.busy).accessibilityLabel(strings("%@ 会话操作", session.name))
                .accessibilityIdentifier("sessionActions-\(session.sessionId)")
                .alert(strings("删除会话“%@”？", deleting?.name ?? session.name), isPresented: Binding(
                    get: { deleting?.sessionId == session.sessionId },
                    set: { if !$0, deleting?.sessionId == session.sessionId { deleting = nil } }
                ), presenting: deleting) { target in
                    Button(strings("取消"), role: .cancel) { deleting = nil }
                        .accessibilityIdentifier("cancelDeleteSession")
                    Button(strings("删除"), role: .destructive) { Task { await model.delete(target) } }
                        .accessibilityIdentifier("confirmDeleteSession")
                } message: { _ in Text(strings("这会结束该会话及其中运行的进程。")) }
        }.foregroundStyle(current ? AppTheme.onPrimaryContainer : AppTheme.foreground)
            .listRowBackground(current ? AppTheme.primaryContainer : nil)
    }

    private var connectionKind: ConnectionNotice.Kind? {
        ConnectionNotice.kind(state: model.frame?.state, code: model.errorCode ?? model.frame?.error,
                              hasError: model.error != nil || model.frame?.error != nil)
    }

    private var connectionStatus: some View {
        Group {
            if model.connecting {
                VStack(spacing: 16) {
                    ProgressView(strings("连接中…"))
                    Button(strings("取消")) { model.goHome() }
                }
            } else {
                ContentUnavailableView {
                    Label(strings(connectionKind == .ended ? "会话已结束" : "连接未就绪"), systemImage: "network.slash")
                } description: {
                    Text(strings(connectionKind == .ended ? "会话已结束，请选择其他会话。" : "请选择会话或重试连接。"))
                } actions: {
                    Button(strings("会话列表")) { openPanel() }
                    if connectionKind != .ended {
                        Button(model.needsTakeover ? strings("接管") : strings("重试")) {
                            if model.needsTakeover { model.showConnectionNotice() }
                            else { Task { await model.retry() } }
                        }.accessibilityIdentifier("connectionRetry")
                    }
                }.disabled(model.busy)
            }
        }
    }

    private func key(_ title: String, _ key: NativeKey) -> some View {
        Button(title) { model.key(key) }.font(.system(size: 13, weight: .medium))
            .frame(maxWidth: .infinity).frame(height: 40).disabled(!model.inputReady)
    }
    private func arrow(_ symbol: String, _ key: NativeKey, _ label: String) -> some View {
        Button { model.key(key) } label: {
            Image(systemName: symbol).font(.system(size: 16, weight: .regular)).frame(maxWidth: .infinity).frame(height: 40)
        }.disabled(!model.inputReady).accessibilityLabel(label)
    }
}

private struct SessionEditor: View {
    @ObservedObject var model: AppModel
    private var strings: AppStrings { model.strings }
    let session: NativeSession?
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var directory = ""

    var body: some View {
        NavigationStack {
            Form {
                TextField(strings("名称"), text: $name).textInputAutocapitalization(.never).autocorrectionDisabled()
                    .accessibilityIdentifier("sessionName")
                if session == nil {
                    TextField(strings("目录（可选）"), text: $directory).textInputAutocapitalization(.never).autocorrectionDisabled()
                        .accessibilityIdentifier("sessionDirectory")
                }
                if let error = model.error { Text(error).font(.footnote).foregroundStyle(AppTheme.error) }
                Button {
                    Task {
                        let result: Bool
                        if let session { result = await model.rename(session, name: name) }
                        else { result = await model.create(name, directory: directory) }
                        if result { dismiss() }
                    }
                } label: { HStack { Text(session == nil ? strings("创建") : strings("保存")); Spacer(); if model.busy { ProgressView() } } }
                    .disabled(model.busy || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("sessionEditorSubmit")
            }
            .navigationTitle(session == nil ? strings("新建会话") : strings("重命名")).navigationBarTitleDisplayMode(.inline)
            .toolbar { Button(strings("取消")) { dismiss() }.disabled(model.busy).accessibilityIdentifier("cancelSessionEditor") }
            .interactiveDismissDisabled(model.busy)
        }.onAppear { name = session?.name ?? ""; model.error = nil }
    }
}

struct TerminalSurface: UIViewRepresentable {
    @ObservedObject var model: AppModel
    private var strings: AppStrings { model.strings }
    @Binding var keyboard: Bool
    var acceptsKeyboard: Bool

    func makeUIView(context: Context) -> TerminalContainer {
        let view = TerminalContainer()
        model.onInputActivity = { [weak view] in view?.gestures.cancel() }
        view.onSize = { [weak model] in model?.resize($0) }
        view.onPointer = { [weak model] in model?.pointer($0) }
        view.onGeometryInvalidated = { [weak model] in model?.invalidatePointerGeometry() }
        view.onSelectionError = { [weak model] in model?.error = model?.strings("所选内容已失效，请重新选择后复制。") }
        view.onHistoryError = { [weak model] in model?.error = $0 }
        view.input.onText = { [weak model] in model?.text($0, paste: $1) }
        view.input.onKey = { [weak model] in model?.key($0) }
        view.input.onHardware = { [weak model] in model?.hardware($0) ?? false }
        view.input.pendingModifiers = { [weak model] in (model?.control == true ? 4 : 0) | (model?.alt == true ? 2 : 0) }
        view.input.onFocusChange = { [binding = $keyboard] visible in
            DispatchQueue.main.async { binding.wrappedValue = visible }
        }
        return view
    }

    func updateUIView(_ view: TerminalContainer, context: Context) {
        view.setLanguage(model.preferences.language)
        view.setFontSize(CGFloat(model.preferences.fontSize))
        view.update(model.frame, terminal: model.terminal)
        view.input.admission(ready: model.inputReady, epoch: model.frame?.inputEpoch)
        view.setKeyboard(keyboard, active: acceptsKeyboard)
    }

    static func dismantleUIView(_ view: TerminalContainer, coordinator: ()) {
        view.setKeyboard(false, active: false)
        view.invalidateCoordinates()
        view.selection.clear()
    }
}
