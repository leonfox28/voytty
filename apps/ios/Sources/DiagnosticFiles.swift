import Foundation
import Darwin

/// The Rust recorder calls this on its worker. Callbacks never re-enter Rust.
/// All managed paths are relative to a verified directory descriptor; no symlinks,
/// shared files or group/world-readable files are accepted.
final class DiagnosticFiles: DiagnosticSink, @unchecked Sendable {
    let directory: URL
    let limit: Int
    private let fileProtection: @Sendable (Int32) throws -> Void
    private let lock = NSLock()
    private let names = ["events.jsonl.1", "events.jsonl", "detail.jsonl.1", "detail.jsonl"]
    private static let off = DiagnosticControl(schema: 1, startedMs: 0, deadlineMs: 0)
    private struct Control: Codable { let schema: UInt8; let started_ms: Int64; let deadline_ms: Int64 }
    enum Failure: Error { case storage }

    init(directory: URL, limit: Int = 2 * 1024 * 1024,
         fileProtection: @escaping @Sendable (Int32) throws -> Void = { try DiagnosticFiles.protect($0) }) {
        self.directory = directory; self.limit = limit; self.fileProtection = fileProtection
    }

    private func locked<T>(_ body: () throws -> T) rethrows -> T {
        lock.lock(); defer { lock.unlock() }; return try body()
    }
    private func valid(_ value: stat, directory: Bool = false) -> Bool {
        value.st_uid == getuid() && value.st_mode & 0o077 == 0 &&
            value.st_mode & S_IFMT == (directory ? S_IFDIR : S_IFREG) && (directory || value.st_nlink == 1)
    }
    private func root(create: Bool) throws -> Int32? {
        if create {
            try FileManager.default.createDirectory(at: directory.deletingLastPathComponent(), withIntermediateDirectories: true)
            guard mkdir(directory.path, 0o700) == 0 || errno == EEXIST else { throw Failure.storage }
        }
        let fd = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        if fd < 0, !create, errno == ENOENT { return nil }
        guard fd >= 0 else { throw Failure.storage }
        var info = stat()
        guard fstat(fd, &info) == 0, valid(info, directory: true) else { close(fd); throw Failure.storage }
        if create {
            do {
                var url = directory
                var values = URLResourceValues(); values.isExcludedFromBackup = true
                try url.setResourceValues(values)
                try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: directory.path)
            } catch { close(fd); throw error }
        }
        return fd
    }
    private func info(_ name: String, _ root: Int32) throws -> stat? {
        var value = stat()
        if fstatat(root, name, &value, AT_SYMLINK_NOFOLLOW) != 0 {
            if errno == ENOENT { return nil }; throw Failure.storage
        }
        guard valid(value) else { throw Failure.storage }
        return value
    }
    private func file(_ name: String, _ root: Int32, write: Bool = false) throws -> Int32 {
        _ = try info(name, root)
        let fd = openat(root, name, O_NOFOLLOW | O_CLOEXEC | (write ? O_WRONLY | O_CREAT | O_APPEND : O_RDONLY), 0o600)
        guard fd >= 0 else { throw Failure.storage }
        var value = stat()
        guard fstat(fd, &value) == 0, valid(value) else { close(fd); throw Failure.storage }
        if write {
            do { try fileProtection(fd) } catch { close(fd); throw error }
        }
        return fd
    }
    private static func protect(_ fd: Int32) throws {
        // Explicitly cover existing files and every atomic replacement, rather
        // than relying on the enclosing directory's inherited/default class.
        var path = [CChar](repeating: 0, count: Int(MAXPATHLEN))
        guard fcntl(fd, F_GETPATH, &path) == 0 else { throw Failure.storage }
        try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication],
                                              ofItemAtPath: String(cString: path))
    }
    private func read(_ fd: Int32, start: Int, count: Int) throws -> Data {
        var bytes = Data(count: count)
        var used = 0
        try bytes.withUnsafeMutableBytes { buffer in
            while used < count {
                let n = pread(fd, buffer.baseAddress!.advanced(by: used), count - used, off_t(start + used))
                if n < 0, errno == EINTR { continue }
                guard n >= 0 else { throw Failure.storage }
                if n == 0 { break }
                used += n
            }
        }
        bytes.count = used
        return bytes
    }
    private func write(_ bytes: Data, _ fd: Int32) throws {
        try bytes.withUnsafeBytes { buffer in
            var used = 0
            while used < bytes.count {
                let n = Darwin.write(fd, buffer.baseAddress!.advanced(by: used), bytes.count - used)
                if n < 0, errno == EINTR { continue }
                guard n > 0 else { throw Failure.storage }; used += n
            }
        }
    }
    private func atomic(_ name: String, _ bytes: Data, _ root: Int32) throws {
        _ = try info(name, root)
        // One recoverable temporary per destination bounds interrupted writes too.
        let temporary = ".write-" + name
        if try info(temporary, root) != nil {
            guard unlinkat(root, temporary, 0) == 0 else { throw Failure.storage }
        }
        let fd = openat(root, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw Failure.storage }
        defer { close(fd); unlinkat(root, temporary, 0) }
        try fileProtection(fd)
        try write(bytes, fd)
        guard fsync(fd) == 0, renameat(root, temporary, root, name) == 0 else { throw Failure.storage }
    }
    private func normalize(_ name: String, _ root: Int32) throws {
        guard let size = try info(name, root)?.st_size, size > limit else { return }
        let fd = try file(name, root); defer { close(fd) }
        let tail = try read(fd, start: Int(size) - limit, count: limit)
        let start = tail.firstIndex(of: 10).map { $0 + 1 } ?? tail.endIndex
        let end = max(start, tail.lastIndex(of: 10).map { $0 + 1 } ?? start)
        try atomic(name, Data(tail[start..<end]), root)
    }
    func append(detail: Bool, record: String) -> Bool {
        (try? locked {
            let bytes = Data(record.utf8)
            guard !bytes.isEmpty, bytes.count <= min(4096, limit), bytes.last == 10,
                  let root = try root(create: true) else { throw Failure.storage }
            defer { close(root) }
            for name in names { try normalize(name, root) }
            let name = detail ? "detail.jsonl" : "events.jsonl"
            if let size = try info(name, root)?.st_size, size + Int64(bytes.count) > limit {
                _ = try info(name + ".1", root)
                guard renameat(root, name, root, name + ".1") == 0 else { throw Failure.storage }
            }
            let fd = try file(name, root, write: true); defer { close(fd) }
            try write(bytes, fd)
            return true
        }) ?? false
    }
    func control() -> DiagnosticControl {
        (try? locked {
            guard let root = try root(create: false) else { return Self.off }; defer { close(root) }
            guard let size = try info("control.json", root)?.st_size, size <= 1024 else { return Self.off }
            let fd = try file("control.json", root); defer { close(fd) }
            let value = try JSONDecoder().decode(Control.self, from: read(fd, start: 0, count: 1025))
            guard value.schema == 1 else { return Self.off }
            return DiagnosticControl(schema: value.schema, startedMs: value.started_ms, deadlineMs: value.deadline_ms)
        }) ?? Self.off
    }
    func setControl(_ value: DiagnosticControl) throws {
        try locked {
            guard let root = try root(create: true) else { throw Failure.storage }; defer { close(root) }
            try atomic("control.json", JSONEncoder().encode(Control(schema: value.schema, started_ms: value.startedMs, deadline_ms: value.deadlineMs)), root)
        }
    }
    func flush() -> Bool {
        (try? locked {
            guard let root = try root(create: false) else { return true }; defer { close(root) }
            for name in names where try info(name, root) != nil {
                let fd = try file(name, root, write: true); defer { close(fd) }
                guard fsync(fd) == 0 else { throw Failure.storage }
            }
            return true
        }) ?? false
    }

    /// Bounded byte snapshots (at most 8 MiB) under the sink lock. Encoding/filtering
    /// runs after unlocking, off Main; rotation cannot change either export pass.
    func export(includeDebug: Bool, lost: UInt64) throws -> Data {
        let snapshots: [(Data, Int)] = try locked {
            guard let root = try root(create: false) else { return [] }; defer { close(root) }
            return try names.prefix(includeDebug ? 4 : 2).compactMap { name in
                guard let size = try info(name, root)?.st_size else { return nil }
                let start = max(0, Int(size) - limit)
                let fd = try file(name, root); defer { close(fd) }
                return (try read(fd, start: start, count: min(Int(size), limit)), start)
            }
        }
        var omitted: UInt64 = 0, truncated: UInt64 = 0
        var records = Data()
        for (bytes, offset) in snapshots {
            truncated += UInt64(offset)
            var start = 0, discard = offset > 0
            for end in bytes.indices where bytes[end] == 10 {
                let length = end - start + 1
                if !discard, length <= 4096, let record = diagnosticExportRecord(line: Data(bytes[start...end])) {
                    records.append(contentsOf: record.utf8)
                } else { omitted += 1 }
                start = end + 1; discard = false
            }
            if start < bytes.count { omitted += 1 }
        }
        var result = Data(diagnosticExportHeader(includeDebug: includeDebug, omitted: omitted, truncatedBytes: truncated, pendingLost: lost).utf8)
        result.append(records)
        return result
    }
    func prepareExport(includeDebug: Bool, lost: UInt64) throws -> URL {
        let bytes = try export(includeDebug: includeDebug, lost: lost)
        try locked {
            guard let root = try root(create: true) else { throw Failure.storage }; defer { close(root) }
            try atomic("zterm-logs.jsonl", bytes, root)
        }
        return directory.appendingPathComponent("zterm-logs.jsonl")
    }
    func removeExport() {
        try? locked {
            guard let root = try root(create: false) else { return }; defer { close(root) }
            if try info("zterm-logs.jsonl", root) != nil { _ = unlinkat(root, "zterm-logs.jsonl", 0) }
            if try info(".write-zterm-logs.jsonl", root) != nil { _ = unlinkat(root, ".write-zterm-logs.jsonl", 0) }
        }
    }
}
