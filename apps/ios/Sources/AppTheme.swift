import SwiftUI

/// The same semantic palette as Android's Theme.kt; controls remain SwiftUI/UIKit.
enum AppTheme {
    static let primary = color(0xA3E07F, 0x39691C)
    static let onPrimary = color(0x0E2005, 0xFFFFFF)
    static let primaryContainer = color(0x233A1A, 0xDCF0CC)
    static let onPrimaryContainer = color(0xC9F4AF, 0x0F2C04)
    static let background = color(0x101713, 0xF5F8F3)
    static let surface = color(0x19221C, 0xFFFFFF)
    static let chrome = color(0x151D18, 0xFAFBF9)
    static let elevated = color(0x222C25, 0xEAEEE8)
    static let foreground = color(0xE8EDE8, 0x151A16)
    static let secondary = color(0x98A69C, 0x59655C)
    static let outline = color(0x28322B, 0xDDE3DB)
    static let success = color(0x7BD66A, 0x2E8B37)
    static let warning = color(0xF0B85A, 0xB7791F)
    static let error = color(0xFF8F84, 0xBA1A1A)
    static let errorContainer = color(0x4A1A15, 0xFFDAD6)

    private static func color(_ dark: UInt32, _ light: UInt32) -> Color {
        Color(uiColor: UIColor { traits in
            let hex = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(red: CGFloat((hex >> 16) & 255) / 255,
                           green: CGFloat((hex >> 8) & 255) / 255,
                           blue: CGFloat(hex & 255) / 255, alpha: 1)
        })
    }
}

struct IconTile: View {
    let symbol: String
    var size: CGFloat = 40
    var body: some View {
        Image(systemName: symbol).font(.system(size: size * 0.48, weight: .medium))
            .frame(width: size, height: size)
            .foregroundStyle(AppTheme.onPrimaryContainer)
            .background(AppTheme.primaryContainer, in: RoundedRectangle(cornerRadius: 12))
    }
}

struct ErrorBanner: View {
    let text: String
    var dismiss: (() -> Void)?
    @Environment(\.locale) private var locale
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Text(text).font(.system(size: 13)).frame(maxWidth: .infinity, alignment: .leading)
            if let dismiss {
                Button(action: dismiss) { Image(systemName: "xmark").frame(width: 24, height: 24) }
                    .accessibilityLabel(AppStrings(language: AppStrings.resolve("system", preferred: [locale.identifier]))("关闭提示"))
            }
        }.padding(12).foregroundStyle(AppTheme.error)
            .background(AppTheme.errorContainer, in: RoundedRectangle(cornerRadius: 12))
    }
}
