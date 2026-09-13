// Adaptive monochrome theme — palette derived from crates/ui/src/theme.rs.
//
// Colors are computed from the same oklch definitions the desktop app uses
// (Björn Ottosson's OKLab matrices, the ones CSS Color 4 specifies), so every
// surface and accent lands on identical sRGB values. **Numbers drive layout,
// colors are paint**: layout constants are plain numbers and never depend on
// which color is painted.

import SwiftUI

enum Theme {
    // ---- paint: neutral surfaces (oklch chroma 0) ----
    /// Main panel background — white / the original #060606.
    static let bg = adaptive(light: .white, dark: grey(6))
    /// Shell / sidebar surface.
    static let surface = adaptive(light: neutral(0.968), dark: grey(13))
    /// Raised surface: popovers, dialogs, cards.
    static let surfaceRaised = adaptive(light: neutral(0.940), dark: neutral(0.235))
    static let sheetPanel = adaptive(light: .white, dark: grey(0x14))
    /// Hover/pressed wash for interactive rows (ink at low alpha).
    static let elementHover = whiteAlpha(0.06)
    /// Active/selected wash.
    static let elementActive = whiteAlpha(0.10)
    /// Hairline border, with a little more contrast on light surfaces.
    static let border = adaptive(light: .black.opacity(0.10), dark: .white.opacity(0.08))
    /// Stronger border for focused/raised edges.
    static let borderStrong = adaptive(light: .black.opacity(0.17), dark: .white.opacity(0.14))

    // ---- paint: text ----
    static let text = adaptive(light: neutral(0.25), dark: neutral(0.922))
    static let textMuted = adaptive(light: neutral(0.439), dark: neutral(0.708))
    static let textFaint = adaptive(light: neutral(0.535), dark: neutral(0.556))

    // ---- paint: accents ----
    static let accent = adaptive(light: oklch(0.511, 0.262, 276.966), dark: oklch(0.673, 0.182, 276.935))
    static let accentStrong = adaptive(light: oklch(0.511, 0.262, 276.966), dark: oklch(0.585, 0.233, 277.117))
    static let danger = adaptive(light: oklch(0.577, 0.245, 27.325), dark: oklch(0.704, 0.191, 22.216))
    static let dangerSoft = adaptive(light: oklch(0.505, 0.213, 27.518), dark: oklch(0.808, 0.114, 19.571))
    static let warning = adaptive(light: oklch(0.555, 0.163, 48.998), dark: oklch(0.828, 0.189, 84.429))

    // ---- paint: status dots (shell/spaces.rs status_dot_color) ----
    static let statusWorking = adaptive(light: oklch(0.592, 0.249, 0.584), dark: oklch(0.718, 0.202, 349.761))
    static let statusCompleted = adaptive(light: oklch(0.508, 0.118, 165.612), dark: oklch(0.765, 0.177, 163.223))
    /// Claude brand orange — kept even on the mono surface.
    static let claudeBrand = Color(red: 0xD9 / 255.0, green: 0x77 / 255.0, blue: 0x57 / 255.0)

    // ---- paint: markdown inline code (violet family) ----
    static let inlineCodeText = adaptive(light: oklch(0.491, 0.241, 292.581), dark: oklch(0.811, 0.111, 293.571))
    static let inlineCodeWash = adaptive(
        light: oklch(0.541, 0.281, 293.009).opacity(0.08),
        dark: oklch(0.702, 0.183, 293.541).opacity(0.12))

    // ---- paint: syntax tokens (soft, paint-only) ----
    static let tokenKeyword = adaptive(light: oklch(0.49, 0.16, 20.0), dark: oklch(0.709, 0.129, 20.0))
    static let tokenString = adaptive(light: oklch(0.46, 0.10, 168.0), dark: oklch(0.770, 0.110, 168.0))
    static let tokenNumber = adaptive(light: oklch(0.47, 0.10, 80.0), dark: oklch(0.780, 0.120, 80.0))

    /// Keep the provider dynamic: SwiftUI and UIKit attributed text must both
    /// resolve against their own view's traits, not a process-global setting.
    static func adaptive(light: Color, dark: Color) -> Color {
        let lightColor = UIColor(light)
        let darkColor = UIColor(dark)
        return Color(uiColor: UIColor { traits in
            traits.userInterfaceStyle == .dark ? darkColor : lightColor
        })
    }

    // ---- numbers drive layout (pt) ----
    static let bubbleRadius: CGFloat = 16
    static let panelRadius: CGFloat = 10
    static let controlRadius: CGFloat = 6
    static let spaceXS: CGFloat = 4
    static let spaceSM: CGFloat = 8
    static let spaceMD: CGFloat = 12
    static let spaceLG: CGFloat = 16
}

// MARK: - Fonts

extension Theme {
    static let fontSansName = "Geist"
    static let fontMonoName = "GeistMono-Regular"

    static func sans(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        // Static weight cuts register as separate families — select by
        // PostScript name so weights actually resolve.
        let name: String
        if weight == .medium {
            name = "Geist-Medium"
        } else if weight == .semibold {
            name = "Geist-SemiBold"
        } else if weight == .bold {
            name = "Geist-Bold"
        } else {
            name = "Geist-Regular"
        }
        return .custom(name, size: size)
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .custom(fontMonoName, size: size).weight(weight)
    }

    static func sansUI(_ size: CGFloat, weight: UIFont.Weight = .regular) -> UIFont {
        let traits: [UIFontDescriptor.TraitKey: Any] = [.weight: weight]
        let descriptor = UIFontDescriptor(fontAttributes: [
            .family: "Geist",
            .traits: traits,
        ])
        return UIFont(descriptor: descriptor, size: size)
    }

    static func monoUI(_ size: CGFloat) -> UIFont {
        UIFont(name: fontMonoName, size: size)
            ?? .monospacedSystemFont(ofSize: size, weight: .regular)
    }
}

// MARK: - Color primitives (ported from theme.rs)

/// A neutral (chroma 0) oklch tone. Chroma 0 means r == g == b exactly.
func neutral(_ lightness: Double) -> Color {
    let v = Double(oklchToSrgb(l: lightness, c: 0, hDeg: 0)[0])
    return Color(red: v, green: v, blue: v)
}

/// Legacy name for the shared hairline/wash primitive: white in dark mode,
/// black in light mode. Image scrims and their labels use explicit colors.
func whiteAlpha(_ alpha: Double) -> Color {
    Theme.adaptive(light: .black.opacity(alpha), dark: .white.opacity(alpha))
}

/// An exact achromatic tone from an 8-bit channel value (`grey(13)` ≡ #0d0d0d).
func grey(_ value: UInt8) -> Color {
    let v = Double(value) / 255.0
    return Color(red: v, green: v, blue: v)
}

/// oklch (CSS notation: L 0..1, C, H degrees) → sRGB Color.
func oklch(_ l: Double, _ c: Double, _ hDeg: Double) -> Color {
    let rgb = oklchToSrgb(l: l, c: c, hDeg: hDeg)
    return Color(red: Double(rgb[0]), green: Double(rgb[1]), blue: Double(rgb[2]))
}

/// oklch → sRGB (each 0..1, clamped/gamut-clipped per channel).
func oklchToSrgb(l: Double, c: Double, hDeg: Double) -> [Double] {
    let h = hDeg * .pi / 180
    let a = c * cos(h)
    let b = c * sin(h)

    // OKLab → LMS (cube roots undone)
    let l_ = l + 0.39633778 * a + 0.21580376 * b
    let m_ = l - 0.105561346 * a - 0.06385417 * b
    let s_ = l - 0.08948418 * a - 1.2914855 * b
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_)

    // LMS → linear sRGB
    let r = 4.0767417 * l3 - 3.3077116 * m3 + 0.23096993 * s3
    let g = -1.268438 * l3 + 2.6097574 * m3 - 0.3413194 * s3
    let bl = -0.0041960863 * l3 - 0.7034186 * m3 + 1.7076147 * s3

    return [gammaEncode(r), gammaEncode(g), gammaEncode(bl)]
}

private func gammaEncode(_ x: Double) -> Double {
    let x = min(max(x, 0), 1)
    return x <= 0.0031308 ? 12.92 * x : 1.055 * pow(x, 1.0 / 2.4) - 0.055
}
