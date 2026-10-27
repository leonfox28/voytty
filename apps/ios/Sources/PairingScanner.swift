import SwiftUI
import AVFoundation
import PhotosUI
import UniformTypeIdentifiers
import CoreImage
import ImageIO

struct PairingCandidate: Identifiable {
    let id = UUID()
    let ticket: String
    let host: NativeHost
}

enum ScanIssue: Error, Equatable {
    case noQR, invalidTicket, unreadableImage, cameraUnavailable
    var key: String {
        switch self {
        case .noQR: return "图片中没有二维码。"
        case .invalidTicket: return "未找到有效的配对码，请在电脑上重新生成。"
        case .unreadableImage: return "无法读取这张图片，请选择其他图片。"
        case .cameraUnavailable: return "相机暂时不可用，请使用图片或手动输入。"
        }
    }
}

enum PairingCodes {
    /// Inspect locally before any network pairing. Distinct tickets for the same
    /// device remain separate choices; the ticket itself is never a UI identifier.
    static func candidates(_ codes: [String], limit: UInt32,
                           inspect: (String) throws -> NativeHost) throws -> [PairingCandidate] {
        guard !codes.isEmpty else { throw ScanIssue.noQR }
        var seen = Set<String>()
        let valid = codes.prefix(64).compactMap { code -> PairingCandidate? in
            guard code.utf8.count <= limit, seen.insert(code).inserted,
                  let host = try? inspect(code) else { return nil }
            return PairingCandidate(ticket: code, host: host)
        }
        guard !valid.isEmpty else { throw ScanIssue.invalidTicket }
        return valid
    }
}

/// Manual entry reverts an over-limit edit. The notice stays until the user changes
/// the restored text; the revert's own change notification must not clear it.
struct TicketDraft {
    var text = ""
    private(set) var restored: String?
    var overLimit: Bool { restored != nil }

    mutating func changed(from old: String, limit: UInt32) {
        if text.utf8.count > limit { restored = old; text = old }
        else if text != restored { restored = nil }
    }
}

enum QRImageReader {
    /// Decode a transformed thumbnail, not a full-resolution photo. The provider's
    /// temporary URL is used only inside its callback and is never persisted.
    static func read(_ url: URL) throws -> [String] {
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard size > 0, size <= 50 * 1024 * 1024,
              let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: 2048,
                kCGImageSourceShouldCacheImmediately: true
              ] as CFDictionary) else { throw ScanIssue.unreadableImage }
        return try codes(in: image)
    }

    static func codes(in image: CGImage) throws -> [String] {
        guard let detector = CIDetector(ofType: CIDetectorTypeQRCode, context: CIContext(),
                                        options: [CIDetectorAccuracy: CIDetectorAccuracyHigh]) else { throw ScanIssue.unreadableImage }
        return detector.features(in: CIImage(cgImage: image)).prefix(64)
            .compactMap { ($0 as? CIQRCodeFeature)?.messageString }
    }
}

@MainActor
final class PairingScannerState: ObservableObject {
    @Published var permission = AVCaptureDevice.authorizationStatus(for: .video)
    @Published var issue: ScanIssue?
    @Published var candidates: [PairingCandidate] = []
    @Published var reading = false
    @Published var manual = false
    @Published var choosingPhoto = false
    @Published var foreground = true
    @Published private(set) var cameraGeneration = UUID()
    private(set) var visible = false
    private var imageRequest: UUID?
    private var progress: Progress?

    var cameraReady: Bool {
        visible && foreground && permission == .authorized && issue == nil && candidates.isEmpty
            && !reading && !manual && !choosingPhoto
    }

    func appear() { visible = true }
    func refreshPermission() {
        permission = AVCaptureDevice.authorizationStatus(for: .video)
        if permission == .notDetermined {
            AVCaptureDevice.requestAccess(for: .video) { [weak self] _ in
                Task { @MainActor in
                    guard let self, self.visible else { return }
                    self.permission = AVCaptureDevice.authorizationStatus(for: .video)
                }
            }
        }
    }
    func reset() { issue = nil; candidates = []; cameraGeneration = UUID() }
    /// Leaving the foreground stops capture and invalidates its late callbacks. A
    /// pending device choice or image result stays; a camera interruption is retried.
    func suspendCamera() {
        if issue == .cameraUnavailable { issue = nil }
        cameraGeneration = UUID()
    }
    func disappear() {
        visible = false; imageRequest = nil; progress?.cancel(); progress = nil
        reading = false; candidates = []; cameraGeneration = UUID()
    }
    func beginImage() -> UUID {
        progress?.cancel(); reset(); reading = true
        let request = UUID(); imageRequest = request
        return request
    }
    func completeImage(_ result: Result<[String], Error>, request: UUID, limit: UInt32,
                       inspect: (String) throws -> NativeHost) -> PairingCandidate? {
        guard visible, request == imageRequest else { return nil }
        imageRequest = nil; progress = nil; reading = false
        switch result {
        case .success(let codes): return receive(codes, limit: limit, inspect: inspect)
        case .failure: issue = .unreadableImage; return nil
        }
    }
    func receive(_ codes: [String], limit: UInt32,
                 inspect: (String) throws -> NativeHost) -> PairingCandidate? {
        guard visible else { return nil }
        do {
            let values = try PairingCodes.candidates(codes, limit: limit, inspect: inspect)
            if values.count == 1 { return values[0] }
            candidates = values
        } catch { issue = error as? ScanIssue ?? .invalidTicket }
        return nil
    }
    func load(_ provider: NSItemProvider, limit: UInt32,
              inspect: @escaping @MainActor @Sendable (String) throws -> NativeHost,
              connect: @escaping @MainActor @Sendable (PairingCandidate) -> Void) {
        let request = beginImage()
        progress = provider.loadFileRepresentation(forTypeIdentifier: UTType.image.identifier) { [weak self] url, _ in
            let result = Result { () throws -> [String] in
                guard let url else { throw ScanIssue.unreadableImage }
                return try QRImageReader.read(url)
            }
            Task { @MainActor in
                guard let self else { return }
                if let candidate = self.completeImage(result, request: request, limit: limit, inspect: inspect) { connect(candidate) }
            }
        }
    }
}

struct ScannerScreen: View {
    @ObservedObject var model: AppModel
    @StateObject private var scanner = PairingScannerState()
    @Environment(\.scenePhase) private var scenePhase
    private var strings: AppStrings { model.strings }
    private var canScan: Bool { scanner.cameraReady && !model.busy && model.error == nil && scenePhase == .active }

    var body: some View {
        VStack(spacing: 0) {
            ZStack {
                Color(red: 0.043, green: 0.055, blue: 0.047)
                if canScan {
                    let generation = scanner.cameraGeneration
                    QRLiveCamera { codes in
                        guard canScan, scanner.cameraGeneration == generation, let runtime = model.runtime else { return }
                        if let candidate = scanner.receive(codes, limit: runtime.ticketTextLimit(), inspect: runtime.inspectTicket) {
                            connect(candidate.ticket)
                        }
                    } failed: {
                        guard canScan, scanner.cameraGeneration == generation else { return }
                        scanner.issue = .cameraUnavailable
                    }
                    .id(generation)
                    ScanCorners().stroke(.white, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                        .aspectRatio(1, contentMode: .fit).padding(36).allowsHitTesting(false)
                    VStack {
                        Text(strings("对准电脑上的配对二维码")).font(.system(size: 14))
                            .padding(12).background(.black.opacity(0.55), in: Capsule()).padding(.top, 20)
                        Spacer()
                    }.foregroundStyle(.white).allowsHitTesting(false)
                }
                if scanner.permission == .denied || scanner.permission == .restricted {
                    VStack(spacing: 16) {
                        Image(systemName: "camera.fill").font(.system(size: 36))
                        Text(strings("需要相机权限才能扫描二维码。"))
                        if scanner.permission == .denied {
                            Button(strings("打开设置")) {
                                if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                            }.buttonStyle(.borderedProminent).foregroundStyle(AppTheme.onPrimary)
                        }
                    }.foregroundStyle(.white).multilineTextAlignment(.center).padding(24)
                }
                if model.busy || scanner.reading { ProgressView().tint(.white) }
                if let error = scanner.issue.map({ strings($0.key) }) ?? model.error, !scanner.manual {
                    VStack {
                        Spacer()
                        VStack(spacing: 12) {
                            Text(error).foregroundStyle(AppTheme.error)
                            Button(strings("重试")) { model.error = nil; scanner.reset() }.buttonStyle(.borderedProminent).foregroundStyle(AppTheme.onPrimary)
                        }.padding(20).frame(maxWidth: .infinity).background(AppTheme.surface, in: RoundedRectangle(cornerRadius: 20))
                    }.padding(16)
                }
            }.clipShape(RoundedRectangle(cornerRadius: 24)).padding(.horizontal, 16)
            HStack(spacing: 12) {
                Button { model.error = nil; scanner.reset(); scanner.choosingPhoto = true } label: {
                    Label(strings("选择图片"), systemImage: "photo").frame(maxWidth: .infinity).frame(height: 40)
                }.accessibilityIdentifier("chooseQRImage")
                Button { model.error = nil; scanner.reset(); scanner.manual = true } label: {
                    Label(strings("手动输入"), systemImage: "keyboard").frame(maxWidth: .infinity).frame(height: 40)
                }.accessibilityIdentifier("enterPairingTicket")
            }.font(.system(size: 14)).buttonStyle(.bordered).disabled(model.busy || scanner.reading).padding(16)
        }
        .navigationTitle(strings("扫描二维码"))
        .onAppear { scanner.appear(); scanner.refreshPermission() }
        .onDisappear { scanner.disappear() }
        .onChange(of: scenePhase) { _, phase in
            scanner.foreground = phase == .active
            if phase == .active { scanner.refreshPermission() }
            else { scanner.suspendCamera() }
        }
        .sheet(isPresented: $scanner.choosingPhoto) {
            NativePhotoPicker { provider in
                scanner.choosingPhoto = false
                guard let provider, let runtime = model.runtime, scanner.visible else { return }
                scanner.load(provider, limit: runtime.ticketTextLimit(), inspect: { try runtime.inspectTicket(ticket: $0) }) { candidate in connect(candidate.ticket) }
            }
        }
        .sheet(isPresented: $scanner.manual) {
            ManualTicketSheet(model: model) { value in
                if await model.pair(value), let host = model.selectedHost {
                    scanner.manual = false
                    await model.select(host)
                }
            }
        }
        .sheet(isPresented: Binding(get: { !scanner.candidates.isEmpty }, set: { if !$0 { scanner.reset() } })) {
            NavigationStack {
                List(scanner.candidates) { candidate in
                    Button {
                        scanner.candidates = []
                        connect(candidate.ticket)
                    } label: {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(candidate.host.name)
                            Text(String(candidate.host.deviceId.suffix(8))).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary)
                        }.padding(.vertical, 4)
                    }
                }.navigationTitle(strings("选择设备")).navigationBarTitleDisplayMode(.inline)
                    .toolbar { Button(strings("取消")) { scanner.reset() } }
            }.presentationDetents([.medium, .large])
        }
    }

    private func connect(_ ticket: String) {
        // Pause capture synchronously, before the async pairing task starts.
        scanner.reading = true
        Task {
            let paired = await model.pair(ticket)
            scanner.reading = false
            guard scanner.visible, paired, let host = model.selectedHost else { return }
            await model.select(host)
        }
    }
}

private struct ScanCorners: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        let arm = min(rect.width, rect.height) * 0.12
        for (x, dx) in [(rect.minX, 1.0), (rect.maxX, -1.0)] {
            for (y, dy) in [(rect.minY, 1.0), (rect.maxY, -1.0)] {
                path.move(to: CGPoint(x: x + dx * arm, y: y))
                path.addLine(to: CGPoint(x: x, y: y))
                path.addLine(to: CGPoint(x: x, y: y + dy * arm))
            }
        }
        return path
    }
}

private struct ManualTicketSheet: View {
    @ObservedObject var model: AppModel
    let connect: (String) async -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var draft = TicketDraft()
    private var strings: AppStrings { model.strings }
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(strings("在电脑上运行")).foregroundStyle(.secondary)
                    Text("zterm pair create").font(.body.monospaced()).textSelection(.enabled)
                    Text(strings("粘贴完整的一次性配对码。")).font(.footnote).foregroundStyle(.secondary)
                }
                Section(strings("配对码")) {
                    TextEditor(text: $draft.text).font(.footnote.monospaced()).frame(minHeight: 130)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().disabled(model.busy)
                        .accessibilityIdentifier("pairingTicket")
                        .onChange(of: draft.text) { old, _ in
                            draft.changed(from: old, limit: model.runtime?.ticketTextLimit() ?? 0)
                        }
                }
                if draft.overLimit { Text(strings("配对码过长，请仅粘贴完整配对码。")).accessibilityIdentifier("pairingTicketTooLong") }
                if let error = model.error { Text(error).foregroundStyle(AppTheme.error) }
                Button { Task { await connect(draft.text); if model.route == .terminal { draft = TicketDraft(); dismiss() } } } label: {
                    HStack { Text(strings("配对并连接")); Spacer(); if model.busy { ProgressView() } }
                }.disabled(draft.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.busy)
            }.navigationTitle(strings("手动输入")).navigationBarTitleDisplayMode(.inline)
                .toolbar { Button(strings("取消")) { draft = TicketDraft(); model.error = nil; dismiss() }.disabled(model.busy) }
                .interactiveDismissDisabled(model.busy)
        }.onDisappear { draft = TicketDraft() }
    }
}

private struct QRLiveCamera: UIViewRepresentable {
    let recognized: ([String]) -> Void
    let failed: () -> Void
    func makeCoordinator() -> CameraCapture { CameraCapture(recognized: recognized, failed: failed) }
    func makeUIView(context: Context) -> CameraPreview {
        let preview = CameraPreview()
        preview.preview.session = context.coordinator.session
        preview.preview.videoGravity = .resizeAspectFill
        context.coordinator.start()
        return preview
    }
    func updateUIView(_ view: CameraPreview, context: Context) {}
    static func dismantleUIView(_ view: CameraPreview, coordinator: CameraCapture) { coordinator.stop() }
}

private final class CameraPreview: UIView {
    override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
    var preview: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
    override func layoutSubviews() {
        super.layoutSubviews()
        guard let connection = preview.connection else { return }
        let angle: CGFloat
        switch window?.windowScene?.interfaceOrientation {
        case .landscapeLeft: angle = 0
        case .landscapeRight: angle = 180
        case .portraitUpsideDown: angle = 270
        default: angle = 90
        }
        if connection.isVideoRotationAngleSupported(angle) { connection.videoRotationAngle = angle }
    }
}

/// Capture configuration, start/stop and delegate delivery share one serial queue.
/// Only the preview layer receives the session on main; callbacks return to main.
private final class CameraCapture: NSObject, AVCaptureMetadataOutputObjectsDelegate, @unchecked Sendable {
    let session = AVCaptureSession()
    private let queue = DispatchQueue(label: "zterm.qr.camera")
    private var active = false
    private var notifications: [NSObjectProtocol] = []
    private let recognized: @MainActor ([String]) -> Void
    private let failed: @MainActor () -> Void
    init(recognized: @escaping @MainActor ([String]) -> Void, failed: @escaping @MainActor () -> Void) {
        self.recognized = recognized; self.failed = failed
    }
    func start() {
        queue.async { [self] in
            active = true
            do {
                guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) else { throw ScanIssue.cameraUnavailable }
                let input = try AVCaptureDeviceInput(device: camera)
                let output = AVCaptureMetadataOutput()
                session.beginConfiguration()
                guard session.canAddInput(input), session.canAddOutput(output) else {
                    session.commitConfiguration(); throw ScanIssue.cameraUnavailable
                }
                session.addInput(input); session.addOutput(output)
                guard output.availableMetadataObjectTypes.contains(.qr) else {
                    session.commitConfiguration(); throw ScanIssue.cameraUnavailable
                }
                output.metadataObjectTypes = [.qr]
                output.setMetadataObjectsDelegate(self, queue: queue)
                session.commitConfiguration()
                for name in [AVCaptureSession.runtimeErrorNotification, AVCaptureSession.wasInterruptedNotification] {
                    notifications.append(NotificationCenter.default.addObserver(forName: name, object: session, queue: nil) { [weak self] _ in
                        self?.queue.async { [weak self] in
                            guard let self, self.active else { return }
                            self.active = false; self.session.stopRunning()
                            Task { @MainActor [failed = self.failed] in failed() }
                        }
                    })
                }
                session.startRunning()
                if !session.isRunning { throw ScanIssue.cameraUnavailable }
            } catch {
                active = false; session.stopRunning()
                Task { @MainActor [failed] in failed() }
            }
        }
    }
    func stop() {
        queue.async { [self] in
            active = false; session.stopRunning()
            notifications.forEach(NotificationCenter.default.removeObserver); notifications = []
        }
    }
    func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) {
        guard active else { return }
        let codes = objects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
        guard !codes.isEmpty else { return }
        active = false; session.stopRunning()
        Task { @MainActor [recognized] in recognized(codes) }
    }
}
