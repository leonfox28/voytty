import Foundation

/// Explicit bundles also localize UIKit controls and asynchronous errors. No global
/// AppleLanguages override: the system photo picker and Copy menu remain system UI.
struct AppStrings: Equatable {
    var language = "system"

    static func resolve(_ language: String, preferred: [String] = Locale.preferredLanguages) -> String {
        if language == "zh" || language == "en" { return language }
        return preferred.lazy.map { $0.lowercased().split(separator: "-").first.map(String.init) ?? "" }
            .first { $0 == "zh" || $0 == "en" } ?? "en"
    }

    var locale: Locale { Locale(identifier: Self.resolve(language)) }
    var bundle: Bundle {
        let name = Self.resolve(language) == "zh" ? "zh-Hans" : "en"
        return Bundle.main.path(forResource: name, ofType: "lproj").flatMap(Bundle.init(path:)) ?? .main
    }

    func callAsFunction(_ key: String, _ arguments: CVarArg...) -> String {
        let format = bundle.localizedString(forKey: key, value: key, table: "Localizable")
        return arguments.isEmpty ? format : String(format: format, locale: locale, arguments: arguments)
    }
}
