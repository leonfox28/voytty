import XCTest
import CoreImage
import CoreText
import ImageIO
import UniformTypeIdentifiers
@testable import Zterm

@MainActor
final class PairingPreferencesTests: XCTestCase {
    private func host(_ value: String) -> NativeHost {
        NativeHost(deviceId: String(repeating: value, count: 64), name: "Host \(value)", relayUrls: [])
    }

    func testExistingAppearancePreferencesGainLanguageWithoutLosingSettings() throws {
        let bytes = Data(#"{"theme":"dark","fontSize":18}"#.utf8)
        let old = try JSONDecoder().decode(AppPreferences.self, from: bytes)
        XCTAssertEqual(old.language, "system")
        XCTAssertEqual(old.theme, "dark")
        XCTAssertEqual(old.fontSize, 18)
        var updated = old; updated.language = "en"
        XCTAssertEqual(try JSONDecoder().decode(AppPreferences.self, from: JSONEncoder().encode(updated)), updated)
        let invalid = try JSONDecoder().decode(AppPreferences.self, from: Data(#"{"language":"fr","theme":"unknown","fontSize":99}"#.utf8))
        XCTAssertEqual(invalid, AppPreferences())
        XCTAssertEqual(AppStrings.resolve("system", preferred: ["fr-FR", "zh-Hant-TW", "en-US"]), "zh")
        XCTAssertEqual(AppStrings.resolve("en", preferred: ["zh-Hans"]), "en")
        XCTAssertEqual(AppStrings.resolve("system", preferred: ["de-DE"]), "en")
    }

    func testPackagedTranslationsHaveMatchingKeysAndFormatArguments() throws {
        func table(_ name: String) throws -> [String: String] {
            let path = try XCTUnwrap(Bundle.main.path(forResource: name, ofType: "lproj"))
            let data = try Data(contentsOf: URL(fileURLWithPath: path).appendingPathComponent("Localizable.strings"))
            return try XCTUnwrap(PropertyListSerialization.propertyList(from: data, format: nil) as? [String: String])
        }
        let english = try table("en"), chinese = try table("zh-Hans")
        XCTAssertGreaterThan(english.count, 100)
        XCTAssertEqual(Set(english.keys), Set(chinese.keys))
        for key in english.keys {
            XCTAssertEqual(english[key]!.components(separatedBy: "%@").count, chinese[key]!.components(separatedBy: "%@").count, key)
        }
        XCTAssertEqual(AppStrings(language: "en")("设置"), "Settings")
        XCTAssertEqual(AppStrings(language: "zh")("设置"), "设置")
        XCTAssertEqual(AppStrings(language: "en")("删除会话“%@”？", "中文-session"), "Delete session “中文-session”?")
        XCTAssertEqual(AppStrings(language: "zh")("删除会话“%@”？", "中文-session"), "删除会话“中文-session”？")
    }

    func testScannedTicketsAreBoundedValidatedDeduplicatedAndRequireChoice() throws {
        var inspected: [String] = []
        let inspect: (String) throws -> NativeHost = { value in
            inspected.append(value)
            if value == "valid-one" { return self.host("a") }
            if value == "valid-two" { return self.host("b") }
            throw ScanIssue.invalidTicket
        }
        let candidates = try PairingCodes.candidates(["not-a-ticket", "valid-one", "valid-one", "valid-two", String(repeating: "中", count: 10)], limit: 20, inspect: inspect)
        XCTAssertEqual(candidates.map(\.ticket), ["valid-one", "valid-two"])
        XCTAssertEqual(inspected, ["not-a-ticket", "valid-one", "valid-two"])
        XCTAssertThrowsError(try PairingCodes.candidates([], limit: 20, inspect: inspect)) { XCTAssertEqual($0 as? ScanIssue, .noQR) }
        XCTAssertThrowsError(try PairingCodes.candidates(["https://example.com"], limit: 20, inspect: inspect)) { XCTAssertEqual($0 as? ScanIssue, .invalidTicket) }
    }

    func testStaleImageCompletionCannotOfferOrPairADevice() {
        let state = PairingScannerState()
        state.appear()
        let old = state.beginImage(), current = state.beginImage()
        var inspections = 0
        let inspect: (String) throws -> NativeHost = { _ in inspections += 1; return self.host("a") }
        XCTAssertNil(state.completeImage(.success(["old"]), request: old, limit: 50, inspect: inspect))
        XCTAssertTrue(state.reading)
        state.disappear()
        XCTAssertNil(state.completeImage(.success(["current"]), request: current, limit: 50, inspect: inspect))
        XCTAssertEqual(inspections, 0)
        XCTAssertTrue(state.candidates.isEmpty)
        state.appear()
        let next = state.beginImage()
        let selected = state.completeImage(.success(["current", "current"]), request: next, limit: 50, inspect: inspect)
        XCTAssertEqual(selected?.ticket, "current")
        XCTAssertEqual(inspections, 1)
        XCTAssertFalse(state.reading)
        XCTAssertNil(state.issue)
    }

    func testSystemDetectorReadsMultipleQRImagesAndRejectsUnreadableFiles() throws {
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 900, height: 480), format: {
            let format = UIGraphicsImageRendererFormat(); format.scale = 1; return format
        }())
        let image = renderer.image { context in
            UIColor.white.setFill(); context.fill(CGRect(x: 0, y: 0, width: 900, height: 480))
            for (index, code) in ["fixture-code-one", "fixture-code-two"].enumerated() {
                let qr = try! Self.qr(code)
                context.cgContext.interpolationQuality = .none
                UIImage(cgImage: qr).draw(in: CGRect(x: 30 + index * 450, y: 45, width: 390, height: 390))
            }
        }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("codes.png")
        try XCTUnwrap(image.pngData()).write(to: url)
        XCTAssertEqual(Set(try QRImageReader.read(url)), ["fixture-code-one", "fixture-code-two"])
        let blank = renderer.image { context in UIColor.white.setFill(); context.fill(CGRect(x: 0, y: 0, width: 900, height: 480)) }
        XCTAssertTrue(try QRImageReader.codes(in: XCTUnwrap(blank.cgImage)).isEmpty)
        try Data("not an image".utf8).write(to: url)
        XCTAssertThrowsError(try QRImageReader.read(url))
    }

    static func qr(_ text: String) throws -> CGImage {
        let filter = try XCTUnwrap(CIFilter(name: "CIQRCodeGenerator"))
        filter.setValue(Data(text.utf8), forKey: "inputMessage")
        filter.setValue("M", forKey: "inputCorrectionLevel")
        let output = try XCTUnwrap(filter.outputImage).transformed(by: CGAffineTransform(scaleX: 5, y: 5))
        return try XCTUnwrap(CIContext().createCGImage(output, from: output.extent))
    }

    func testBundledFontFacesKeepCellGeometryAndOwnCJKAndNerdGlyphs() throws {
        let singles = ["M", "i", "─", "│", "●", "e\u{0301}", "\u{e0a0}", "\u{e0b0}", "\u{e5fa}", "\u{e7a8}", "\u{f115}", "\u{f418}", "\u{ea60}", "\u{e300}", "\u{e000}", "\u{f0001}"]
        for size in 10...20 {
            let normal = TerminalFont.font(size: CGFloat(size))
            let width = TerminalFont.advance("M", font: normal)
            for style in 0..<4 {
                let font = TerminalFont.font(size: CGFloat(size), bold: style & 1 != 0, italic: style & 2 != 0)
                XCTAssertEqual(font.fontName, TerminalFont.names[style])
                XCTAssertEqual(font.ascender, normal.ascender, accuracy: 0.01)
                XCTAssertEqual(font.descender, normal.descender, accuracy: 0.01)
                for text in singles + ["中", "文", "终", "端", "繁", "體"] {
                    let expected = singles.contains(text) ? width : width * 2
                    XCTAssertEqual(TerminalFont.advance(text, font: font), expected, accuracy: 0.01, "\(font.fontName) \(text) \(size)")
                    let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: [.font: font]))
                    let runs = CTLineGetGlyphRuns(line) as! [CTRun]
                    XCTAssertFalse(runs.isEmpty)
                    for run in runs {
                        let attributes = CTRunGetAttributes(run) as NSDictionary
                        let face = try XCTUnwrap(attributes[kCTFontAttributeName]) as! CTFont
                        XCTAssertEqual(CTFontCopyPostScriptName(face) as String, TerminalFont.names[style], "No system fallback for \(text)")
                    }
                }
            }
        }
        XCTAssertNotNil(Bundle.main.url(forResource: "LXGWBrightCode-OFL-1.1", withExtension: "txt", subdirectory: "licenses"))
    }

    func testProjectLicenseTextsAreBundled() throws {
        for (name, heading) in [
            ("Zterm-NOTICE", "Required Notice: Copyright 2026 Leon"),
            ("Zterm-PolyForm-Noncommercial-1.0.0", "# PolyForm Noncommercial License 1.0.0"),
            ("Zterm-Commercial-License", "# zterm Commercial Use License")
        ] {
            let url = try XCTUnwrap(Bundle.main.url(forResource: name, withExtension: "txt", subdirectory: "licenses"))
            let text = try String(contentsOf: url, encoding: .utf8)
            XCTAssertTrue(text.hasPrefix(heading), name)
        }
    }
}
