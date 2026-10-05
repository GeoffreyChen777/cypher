// Stroked UI glyphs — the desktop's hand-drawn Solar-Linear-style icons
// (crates/ui/assets/icons), rendered natively: same path data, stroked at the
// same 1.5/24 weight with round caps, tinted by the foreground style.

import SwiftUI

enum LineIcon {
    case gitBranch
    case folder
    case folderWithFiles
    // The composer `/` menu's command glyphs (settings/commands.rs `icon`).
    case foldVertical
    case archiveUp
    case flag
    case bolt
    case code
    case hierarchy
    case users
    case pulse
    case restart
    case book
    case command
    case chevronRight
    // The composer `@` menu's rows (composer.rs render_mention_popup).
    case document
    case chatRoundLine

    /// (paths, circles cx/cy/r) in a 24×24 viewbox.
    var elements: (paths: [String], circles: [(CGFloat, CGFloat, CGFloat)]) {
        switch self {
        case .gitBranch:
            return (
                paths: [
                    "M6.5 7.75v8.5",
                    "M17.5 9.75c0 2.9-2.6 4.35-6.2 4.72c-1.9.2-3.3.9-4 2.03",
                ],
                circles: [(6.5, 5.5, 2.25), (6.5, 18.5, 2.25), (17.5, 7.5, 2.25)]
            )
        case .folder:
            return (
                paths: [
                    "M18 10h-5",
                    "M2 6.95c0-.883 0-1.324.07-1.692A4 4 0 0 1 5.257 2.07C5.626 2 6.068 2 6.95 2c.386 0 .58 0 .766.017a4 4 0 0 1 2.18.904c.144.119.28.255.554.529L11 4c.816.816 1.224 1.224 1.712 1.495a4 4 0 0 0 .848.352C14.098 6 14.675 6 15.828 6h.374c2.632 0 3.949 0 4.804.77q.119.105.224.224c.77.855.77 2.172.77 4.804V14c0 3.771 0 5.657-1.172 6.828S17.771 22 14 22h-4c-3.771 0-5.657 0-6.828-1.172S2 17.771 2 14z",
                ],
                circles: []
            )
        case .folderWithFiles:
            return (
                paths: [
                    "M18 10h-5",
                    "M10 3h6.5c.464 0 .697 0 .892.026a3 3 0 0 1 2.582 2.582c.026.195.026.428.026.892",
                    "M2 6.95c0-.883 0-1.324.07-1.692A4 4 0 0 1 5.257 2.07C5.626 2 6.068 2 6.95 2c.386 0 .58 0 .766.017a4 4 0 0 1 2.18.904c.144.119.28.255.554.529L11 4c.816.816 1.224 1.224 1.712 1.495a4 4 0 0 0 .848.352C14.098 6 14.675 6 15.828 6h.374c2.632 0 3.949 0 4.804.77q.119.105.224.224c.77.855.77 2.172.77 4.804V14c0 3.771 0 5.657-1.172 6.828S17.771 22 14 22h-4c-3.771 0-5.657 0-6.828-1.172S2 17.771 2 14z",
                ],
                circles: []
            )
        case .foldVertical:
            return (paths: ["M7 3.5l5 4.5 5-4.5M7 20.5l5-4.5 5 4.5M5 12h14"], circles: [])
        case .archiveUp:
            return (
                paths: [
                    "M2 12c0-4.714 0-7.071 1.464-8.536C4.93 2 7.286 2 12 2s7.071 0 8.535 1.464C22 4.93 22 7.286 22 12",
                    "M2 14c0-2.8 0-4.2.545-5.27A5 5 0 0 1 4.73 6.545C5.8 6 7.2 6 10 6h4c2.8 0 4.2 0 5.27.545a5 5 0 0 1 2.185 2.185C22 9.8 22 11.2 22 14s0 4.2-.545 5.27a5 5 0 0 1-2.185 2.185C18.2 22 16.8 22 14 22h-4c-2.8 0-4.2 0-5.27-.545a5 5 0 0 1-2.185-2.185C2 18.2 2 16.8 2 14Z",
                    "M12 16.5V10m0 0l-2.5 2.5M12 10l2.5 2.5",
                ],
                circles: []
            )
        case .flag:
            return (paths: ["M5 21.5V3", "M5 4h12l-2.75 4.25L17 12.5H5"], circles: [])
        case .bolt:
            return (paths: ["M13.5 2.5L5 13.5h6.5l-1 8l8.5-11h-6.5z"], circles: [])
        case .code:
            return (paths: ["M8 7l-5 5l5 5m8-10l5 5l-5 5M13.5 4.5l-3 15"], circles: [])
        case .hierarchy:
            return (
                paths: ["M12 7.5V12m-6.5 4.5V14a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v2.5"],
                circles: [(12, 5, 2.5), (5.5, 19, 2.5), (18.5, 19, 2.5)]
            )
        case .users:
            return (
                paths: [
                    "M2.5 20.5c0-3.59 2.91-6 6.5-6s6.5 2.41 6.5 6",
                    "M15.5 3.75a3.25 3.25 0 0 1 0 6.5m2.5 4.5c2.03.6 3.5 2.6 3.5 5.25",
                ],
                circles: [(9, 7, 3.5)]
            )
        case .pulse:
            return (paths: ["M2 12h4l2.5-6l4 12l2.5-6H22"], circles: [])
        case .restart:
            return (paths: ["M4.27 11a8 8 0 1 1 2.07 4.66", "M6.84 20.66l-.63-2.86l2.86-.63"], circles: [])
        case .book:
            return (
                paths: [
                    "M4.5 19.5V5A2.5 2.5 0 0 1 7 2.5h12.5v15H7a2.5 2.5 0 0 0-2.5 2.5A2.5 2.5 0 0 0 7 22.5h12.5",
                    "M9 7.5h6",
                ],
                circles: []
            )
        case .command:
            return (
                paths: ["M8 8h8v8H8zm8 8.001h3a3 3 0 1 1-3 3zm-7.999 0h-3a3 3 0 1 0 3 3zM16 8h3a3 3 0 1 0-3-3zM8.001 8h-3a3 3 0 1 1 3-3z"],
                circles: []
            )
        case .chevronRight:
            return (paths: ["m9 5l6 7l-6 7"], circles: [])
        case .document:
            return (
                paths: [
                    "M3 10c0-3.771 0-5.657 1.172-6.828S7.229 2 11 2h2c3.771 0 5.657 0 6.828 1.172S21 6.229 21 10v4c0 3.771 0 5.657-1.172 6.828S16.771 22 13 22h-2c-3.771 0-5.657 0-6.828-1.172S3 17.771 3 14z",
                    "M8 10h8m-8 4h5",
                ],
                circles: []
            )
        case .chatRoundLine:
            return (
                paths: [
                    "M12 22C17.5228 22 22 17.5228 22 12C22 6.47715 17.5228 2 12 2C6.47715 2 2 6.47715 2 12C2 13.5997 2.37562 15.1116 3.04346 16.4525C3.22094 16.8088 3.28001 17.2161 3.17712 17.6006L2.58151 19.8267C2.32295 20.793 3.20701 21.677 4.17335 21.4185L6.39939 20.8229C6.78393 20.72 7.19121 20.7791 7.54753 20.9565C8.88837 21.6244 10.4003 22 12 22Z",
                    "M8 10.5H16",
                    "M8 14H13.5",
                ],
                circles: []
            )
        }
    }
}

struct LineIconShape: Shape {
    let icon: LineIcon

    func path(in rect: CGRect) -> Path {
        var combined = Path()
        let elements = icon.elements
        for data in elements.paths {
            combined.addPath(SVGPathParser.path(from: data))
        }
        for (cx, cy, r) in elements.circles {
            combined.addEllipse(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
        }
        let scale = min(rect.width, rect.height) / 24
        let dx = rect.minX + (rect.width - 24 * scale) / 2
        let dy = rect.minY + (rect.height - 24 * scale) / 2
        return combined.applying(CGAffineTransform(scaleX: scale, y: scale)
            .concatenating(CGAffineTransform(translationX: dx, y: dy)))
    }
}

/// An icon element: `LineIconView(.gitBranch, size: 12, color: …)`.
struct LineIconView: View {
    let icon: LineIcon
    var size: CGFloat = 14
    var color: Color = Theme.textMuted

    init(_ icon: LineIcon, size: CGFloat = 14, color: Color = Theme.textMuted) {
        self.icon = icon
        self.size = size
        self.color = color
    }

    var body: some View {
        LineIconShape(icon: icon)
            .stroke(color, style: StrokeStyle(lineWidth: 1.5 * size / 24,
                                              lineCap: .round, lineJoin: .round))
            .frame(width: size, height: size)
    }
}
