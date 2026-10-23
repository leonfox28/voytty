import Foundation

/// One failed connection attempt or terminal lifecycle event. Its identity lets
/// a late system-alert dismissal leave a newer attempt's notice untouched.
struct ConnectionNotice: Identifiable {
    enum Kind { case failed, occupied, leaseLost, ended }
    let id = UUID()
    let host: String
    let session: String?
    let kind: Kind
    let message: String

    var title: String {
        switch kind {
        case .failed: return "连接失败"
        case .occupied: return "会话已被占用"
        case .leaseLost: return "已在其他设备连接"
        case .ended: return "会话已结束"
        }
    }
    var takeover: Bool { session != nil && [.occupied, .leaseLost].contains(kind) }

    static func kind(state: String?, code: String?, hasError: Bool) -> Kind? {
        if ["active", "synchronizing", "reconnecting"].contains(state ?? "") { return nil }
        if state == "lease_lost" || code == "lease_lost" { return .leaseLost }
        if ["session_occupied", "controller_busy"].contains(code ?? "") { return .occupied }
        if state == "ended" || ["session_ended", "session_not_found"].contains(code ?? "") { return .ended }
        if state == "closed" || hasError { return .failed }
        return nil
    }
}
