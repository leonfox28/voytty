import Foundation

/// A picker may finish after cancellation. The same flag also bounds background reads.
final class UploadCancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false
    private var coordinator: NSFileCoordinator?
    func cancel() {
        let pending = lock.withLock { cancelled = true; return coordinator }
        pending?.cancel()
    }
    func coordinate(with coordinator: NSFileCoordinator?) throws {
        try lock.withLock {
            if coordinator != nil && cancelled { throw CancellationError() }
            self.coordinator = coordinator
        }
    }
    func check() throws { if lock.withLock({ cancelled }) { throw CancellationError() } }
}

/// Owns only our private copy, never the document selected by the user.
final class StagedUpload: Sendable {
    let url: URL
    let filename: String
    let size: UInt64
    let fileExtension: String
    private let directory: URL

    private init(url: URL, filename: String, size: UInt64, fileExtension: String, directory: URL) {
        self.url = url; self.filename = filename; self.size = size
        self.fileExtension = fileExtension; self.directory = directory
    }

    deinit { try? FileManager.default.removeItem(at: directory) }

    /// Run off the main actor. Coordinate external/iCloud documents and copy in
    /// bounded chunks; provider metadata is only an early rejection, not the limit.
    static func copy(_ source: URL, suggestedName: String? = nil, cancellation: UploadCancellation,
                     limit: UInt64 = uploadLimitBytes()) throws -> StagedUpload {
        try cancellation.check()
        let scoped = source.startAccessingSecurityScopedResource()
        defer { if scoped { source.stopAccessingSecurityScopedResource() } }
        var coordinationError: NSError?
        var result: Result<StagedUpload, Error>?
        let coordinator = NSFileCoordinator()
        try cancellation.coordinate(with: coordinator)
        defer { try? cancellation.coordinate(with: nil) }
        coordinator.coordinate(readingItemAt: source, options: [], error: &coordinationError) { readable in
            result = Result {
                try cancellation.check()
                let info = try readable.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
                guard info.isRegularFile == true else { throw NativeError.RequestFailed(code: "upload_source_invalid") }
                if let size = info.fileSize, size > limit { throw NativeError.RequestFailed(code: "upload_too_large") }
                let directory = FileManager.default.temporaryDirectory.appendingPathComponent("voytty-upload-" + UUID().uuidString, isDirectory: true)
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                                                       attributes: [.posixPermissions: 0o700])
                var completed = false
                defer { if !completed { try? FileManager.default.removeItem(at: directory) } }
                let destination = directory.appendingPathComponent("source")
                guard FileManager.default.createFile(atPath: destination.path, contents: nil,
                                                     attributes: [.posixPermissions: 0o600]) else {
                    throw NativeError.RequestFailed(code: "upload_source_invalid")
                }
                let input = try FileHandle(forReadingFrom: readable)
                defer { try? input.close() }
                let output = try FileHandle(forWritingTo: destination)
                defer { try? output.close() }
                var copied: UInt64 = 0
                while true {
                    try cancellation.check()
                    let data = try input.read(upToCount: 65_536) ?? Data()
                    if data.isEmpty { break }
                    guard UInt64(data.count) <= limit - copied else { throw NativeError.RequestFailed(code: "upload_too_large") }
                    try output.write(contentsOf: data)
                    copied += UInt64(data.count)
                }
                try cancellation.check()
                let original = suggestedName.flatMap { $0.isEmpty ? nil : $0 } ?? source.lastPathComponent
                let filename = String(original.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }.prefix(160).map(String.init).joined())
                let ext = (original as NSString).pathExtension
                let file = StagedUpload(url: destination, filename: filename, size: copied,
                                        fileExtension: ext.isEmpty ? source.pathExtension : ext, directory: directory)
                completed = true
                return file
            }
        }
        if let result { return try result.get() }
        throw coordinationError ?? NSError(domain: "voytty.upload.source", code: 1)
    }
}
