import XCTest
import SwiftUI
import UIKit
@testable import Cypher

@MainActor
final class AppearanceTests: XCTestCase {
    private func rgba(_ color: Color, _ style: UIUserInterfaceStyle) -> [CGFloat] {
        rgba(UIColor(color), style)
    }

    private func rgba(_ color: UIColor, _ style: UIUserInterfaceStyle) -> [CGFloat] {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        XCTAssertTrue(color.resolvedColor(with: UITraitCollection(userInterfaceStyle: style))
            .getRed(&r, green: &g, blue: &b, alpha: &a))
        return [r, g, b, a]
    }

    private func contrast(_ foreground: Color, _ background: Color, _ style: UIUserInterfaceStyle) -> Double {
        func luminance(_ rgb: [CGFloat]) -> Double {
            let linear = rgb.prefix(3).map { value -> Double in
                let v = Double(value)
                return v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4)
            }
            return linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
        }
        let fg = rgba(foreground, style), bg = rgba(background, style)
        let blended = (0..<3).map { fg[$0] * fg[3] + bg[$0] * (1 - fg[3]) }
        let a = luminance(blended), b = luminance(bg)
        return (max(a, b) + 0.05) / (min(a, b) + 0.05)
    }

    func testPreferenceDefaultsAndUnknownValuesFollowSystem() {
        XCTAssertNil(AppAppearance(storedValue: "").colorScheme)
        XCTAssertNil(AppAppearance(storedValue: "unknown").colorScheme)
        XCTAssertNil(AppAppearance.system.colorScheme)
        XCTAssertEqual(AppAppearance.light.colorScheme, .light)
        XCTAssertEqual(AppAppearance.dark.colorScheme, .dark)
        for appearance in AppAppearance.allCases {
            XCTAssertEqual(AppAppearance(storedValue: appearance.rawValue), appearance)
        }
    }

    func testDynamicPalettePreservesDarkAndProvidesLightSurfaces() {
        XCTAssertEqual(rgba(Theme.bg, .light)[0], 1, accuracy: 0.001)
        XCTAssertEqual(rgba(Theme.bg, .dark)[0], 6.0 / 255, accuracy: 0.001)
        XCTAssertEqual(rgba(Theme.surface, .dark)[0], 13.0 / 255, accuracy: 0.001)
        XCTAssertEqual(rgba(Theme.sheetPanel, .dark)[0], 20.0 / 255, accuracy: 0.001)
        XCTAssertEqual(rgba(Theme.sheetPanel, .light)[0], 1, accuracy: 0.001)
        XCTAssertEqual(rgba(whiteAlpha(0.06), .light)[0], 0, accuracy: 0.001)
        XCTAssertEqual(rgba(whiteAlpha(0.06), .dark)[0], 1, accuracy: 0.001)
        for style: UIUserInterfaceStyle in [.light, .dark] {
            XCTAssertEqual(rgba(whiteAlpha(0.06), style)[3], 0.06, accuracy: 0.001)
            for surface in [Theme.bg, Theme.surface, Theme.surfaceRaised, Theme.sheetPanel] {
                for text in [Theme.text, Theme.textMuted, Theme.inlineCodeText,
                             Theme.tokenKeyword, Theme.tokenString, Theme.tokenNumber] {
                    XCTAssertGreaterThanOrEqual(contrast(text, surface, style), 4.5)
                }
                XCTAssertGreaterThanOrEqual(contrast(Theme.textFaint, surface, style), 3)
            }
        }
    }

    func testAttributedColorsRemainDynamicWithoutRebuildingText() throws {
        var code = InlineStyle.plain
        code.code = true
        let inline = TranscriptTextStyle.inline([
            InlineRun(text: "text ", style: .plain),
            InlineRun(text: "code", style: code),
        ])
        let foreground = try XCTUnwrap(inline.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? UIColor)
        let codeColor = try XCTUnwrap(inline.attribute(.foregroundColor, at: 5, effectiveRange: nil) as? UIColor)
        let wash = try XCTUnwrap(inline.attribute(.cypherInlineCode, at: 5, effectiveRange: nil) as? UIColor)
        let source = TranscriptTextStyle.code("let x = 1", spans: [[TokenSpan(range: 0..<3, cls: .keyword)]])
        let token = try XCTUnwrap(source.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? UIColor)
        for color in [foreground, codeColor, wash, token] {
            XCTAssertNotEqual(rgba(color, .light), rgba(color, .dark))
        }
        // .opacity / UIColor bridging must not freeze a cached code block's
        // base text color to the trait that was current when it was built.
        let base = try XCTUnwrap(source.attribute(.foregroundColor, at: 4, effectiveRange: nil) as? UIColor)
        XCTAssertNotEqual(rgba(base, .light), rgba(base, .dark))
    }

    func testNativeSelectionSurvivesLiveAppearanceChanges() async throws {
        let attributed = TranscriptTextStyle.inline([InlineRun(text: "Select this text", style: .plain)])
        let host = UIHostingController(rootView: SelectableTranscriptText(attributed: attributed))
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let previous = scene.windows.first(where: \.isKeyWindow)
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer {
            window.isHidden = true
            window.rootViewController = nil
            previous?.makeKey()
        }
        func findText(_ view: UIView) -> TranscriptUITextView? {
            if let text = view as? TranscriptUITextView { return text }
            return view.subviews.lazy.compactMap { findText($0) }.first
        }
        for _ in 0..<30 {
            host.view.layoutIfNeeded()
            if findText(host.view) != nil { break }
            try await Task.sleep(for: .milliseconds(20))
        }
        let view = try XCTUnwrap(findText(host.view))
        view.selectedRange = NSRange(location: 0, length: 6)
        for style: UIUserInterfaceStyle in [.light, .dark, .light] {
            host.overrideUserInterfaceStyle = style
            try await Task.sleep(for: .milliseconds(50))
            XCTAssertEqual(view.traitCollection.userInterfaceStyle, style)
            XCTAssertEqual(view.selectedRange, NSRange(location: 0, length: 6))
            XCTAssertEqual(view.text, attributed.string)
        }
    }
}
