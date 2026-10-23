import Foundation
import Security

struct SavedHost: Codable, Identifiable, Equatable {
    let deviceId: String
    let name: String
    let relayUrls: [String]
    var lastSession: String?
    var id: String { deviceId }
    var native: NativeHost { NativeHost(deviceId: deviceId, name: name, relayUrls: relayUrls) }

    init(_ host: NativeHost) {
        deviceId = host.deviceId
        name = host.name
        relayUrls = host.relayUrls
    }
}

struct SavedState: Codable {
    let seed: Data
    var hosts: [SavedHost]
    var recent: SessionRecord?
    var activeTerminal: SessionRecord?
    var preferences: AppPreferences

    init(seed: Data, hosts: [SavedHost], recent: SessionRecord? = nil, activeTerminal: SessionRecord? = nil,
         preferences: AppPreferences = AppPreferences()) {
        self.seed = seed; self.hosts = hosts; self.recent = recent
        self.activeTerminal = activeTerminal; self.preferences = preferences
    }

    private enum CodingKeys: String, CodingKey { case seed, hosts, recent, activeTerminal, preferences }
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        seed = try values.decode(Data.self, forKey: .seed)
        hosts = try values.decode([SavedHost].self, forKey: .hosts)
        recent = try values.decodeIfPresent(SessionRecord.self, forKey: .recent)
        activeTerminal = try values.decodeIfPresent(SessionRecord.self, forKey: .activeTerminal)
        preferences = try values.decodeIfPresent(AppPreferences.self, forKey: .preferences) ?? AppPreferences()
    }
}

struct SessionRecord: Codable, Equatable {
    let host: String
    let session: String
}

struct AppPreferences: Codable, Equatable {
    var language = "system"
    var theme = "system"
    var fontSize = 15
    var notificationsEnabled = true

    init() {}
    private enum CodingKeys: String, CodingKey { case language, theme, fontSize, notificationsEnabled }
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        language = try values.decodeIfPresent(String.self, forKey: .language) ?? "system"
        theme = try values.decodeIfPresent(String.self, forKey: .theme) ?? "system"
        fontSize = try values.decodeIfPresent(Int.self, forKey: .fontSize) ?? 15
        notificationsEnabled = try values.decodeIfPresent(Bool.self, forKey: .notificationsEnabled) ?? true
        self = validated
    }

    var validated: Self {
        var result = self
        if !["system", "zh", "en"].contains(language) { result.language = "system" }
        if !["system", "dark", "light"].contains(theme) { result.theme = "system" }
        if !(10...20).contains(fontSize) { result.fontSize = 15 }
        return result
    }
}

enum StoreError: LocalizedError {
    case keychain(OSStatus), corrupt
    var errorDescription: String? {
        switch self {
        case .keychain(let status): return "无法访问设备钥匙串（\(status)）。请解锁设备后重试。"
        case .corrupt: return "已保存的设备身份无法读取。为保护现有配对，未生成替代身份。"
        }
    }
}

/// Identity and address book share one atomic Keychain item. No backup or iCloud sync.
struct HostStore {
    var service = (Bundle.main.bundleIdentifier ?? "zterm") + ".controller"
    private var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service, kSecAttrAccount as String: "identity-v1",
         kSecAttrSynchronizable as String: false]
    }

    func load() throws -> SavedState {
        var lookup = query
        lookup[kSecReturnData as String] = true
        lookup[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(lookup as CFDictionary, &result)
        if status == errSecItemNotFound {
            var seed = Data(count: 32)
            let randomStatus = seed.withUnsafeMutableBytes {
                SecRandomCopyBytes(kSecRandomDefault, $0.count, $0.baseAddress!)
            }
            guard randomStatus == errSecSuccess else { throw StoreError.keychain(randomStatus) }
            let state = SavedState(seed: seed, hosts: [])
            var item = query
            item[kSecValueData as String] = try JSONEncoder().encode(state)
            item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            let added = SecItemAdd(item as CFDictionary, nil)
            if added == errSecDuplicateItem { return try load() }
            guard added == errSecSuccess else { throw StoreError.keychain(added) }
            return state
        }
        guard status == errSecSuccess else { throw StoreError.keychain(status) }
        guard let bytes = result as? Data,
              let state = try? JSONDecoder().decode(SavedState.self, from: bytes),
              state.seed.count == 32 else { throw StoreError.corrupt }
        return state
    }

    func save(_ state: SavedState) throws {
        guard state.seed.count == 32 else { throw StoreError.corrupt }
        let attributes = [kSecValueData as String: try JSONEncoder().encode(state)]
        let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        guard status == errSecSuccess else { throw StoreError.keychain(status) }
    }
}
