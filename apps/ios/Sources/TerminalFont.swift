import UIKit
import CoreText

/// The same unmodified LXGW Bright Code NF GB faces packaged by Android.
/// Light/Regular represent terminal normal/bold; neither bold nor italic is synthesized.
enum TerminalFont {
    static let names = ["LXGWBrightCodeGB-Light", "LXGWBrightCodeGB-Regular",
                        "LXGWBrightCodeGB-LightItalic", "LXGWBrightCodeGB-Italic"]

    static func font(size: CGFloat, bold: Bool = false, italic: Bool = false) -> UIFont {
        UIFont(name: names[(bold ? 1 : 0) + (italic ? 2 : 0)], size: size)
            ?? UIFont.monospacedSystemFont(ofSize: size, weight: bold ? .bold : .regular)
    }

    static func advance(_ text: String, font: UIFont) -> CGFloat {
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: [.font: font]))
        return CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
    }
}
