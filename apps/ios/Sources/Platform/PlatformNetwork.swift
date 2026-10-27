import Foundation
import Network

enum PlatformNetwork {
    static func servers() -> [NativeDnsServer] {
        var buffer = [VoyttyDNSServer](repeating: VoyttyDNSServer(), count: 8)
        let count = Int(voytty_dns_servers(&buffer, Int32(buffer.count)))
        return buffer.prefix(count).map { server in
            var address = server.address
            let bytes = withUnsafeBytes(of: &address) { Array($0.prefix(Int(server.length))) }
            return NativeDnsServer(address: Data(bytes), scopeId: server.scope)
        }
    }
}
