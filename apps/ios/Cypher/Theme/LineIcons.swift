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
    /// An artifact patch's card (artifact.rs `render_card`).
    case pen
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
    // A message's strip (transcript render.rs: copy, its confirmation, and
    // the substituted-model warning).
    case copy
    case check
    case dangerTriangle
    /// Permission states on a command's row (transcript.rs
    /// `permission_badge_icon`): the shield with its mark — an arrow leaving
    /// it (ran outside the sandbox), a keyhole (held by it), a cross
    /// (refused), a check (an approved access).
    case shieldOut
    case shieldKeyhole
    case shieldCross
    case shieldCheck

    /// (paths, circles cx/cy/r) in a 24×24 viewbox.
    /// Solar's shield outline, shared by every shield icon.
    static let shieldOutline =
        "M3 10.4167C3 7.21907 3 5.62028 3.37752 5.08241C3.75503 4.54454 5.25832 4.02996 8.26491 3.00079L8.83772 2.80472C10.405 2.26824 11.1886 2 12 2C12.8114 2 13.595 2.26824 15.1623 2.80472L15.7351 3.00079C18.7417 4.02996 20.245 4.54454 20.6225 5.08241C21 5.62028 21 7.21907 21 10.4167C21 10.8996 21 11.4234 21 11.9914C21 17.6294 16.761 20.3655 14.1014 21.5273C13.38 21.8424 13.0193 22 12 22C10.9807 22 10.62 21.8424 9.89856 21.5273C7.23896 20.3655 3 17.6294 3 11.9914C3 11.4234 3 10.8996 3 10.4167Z"

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
        case .pen:
            return (
                paths: [
                    "m14.36 4.079l.927-.927a3.932 3.932 0 0 1 5.561 5.561l-.927.927m-5.56-5.561s.115 1.97 1.853 3.707C17.952 9.524 19.92 9.64 19.92 9.64m-5.56-5.561l-8.522 8.52c-.577.578-.866.867-1.114 1.185a6.6 6.6 0 0 0-.749 1.211c-.173.364-.302.752-.56 1.526l-1.094 3.281m17.6-10.162L11.4 18.16c-.577.577-.866.866-1.184 1.114a6.6 6.6 0 0 1-1.211.749c-.364.173-.751.302-1.526.56l-3.281 1.094m0 0l-.802.268a1.06 1.06 0 0 1-1.342-1.342l.268-.802m1.876 1.876l-1.876-1.876"
                ],
                circles: []
            )
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
                paths: [
                    "M8 8h8v8H8zm8 8.001h3a3 3 0 1 1-3 3zm-7.999 0h-3a3 3 0 1 0 3 3zM16 8h3a3 3 0 1 0-3-3zM8.001 8h-3a3 3 0 1 1 3-3z"
                ],
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
        case .copy:
            return (
                paths: [
                    "M6.6 11.4c0-2.263 0-3.394.703-4.097C8.006 6.6 9.137 6.6 11.4 6.6h4.8c2.263 0 3.394 0 4.097.703c.703.703.703 1.834.703 4.097v4.8c0 2.263 0 3.394-.703 4.097c-.703.703-1.834.703-4.097.703h-4.8c-2.263 0-3.394 0-4.097-.703C6.6 19.594 6.6 18.463 6.6 16.2z",
                    "M17.396 6.6c-.005-1.774-.05-2.707-.657-3.314C16.036 2.583 14.905 2.583 12.642 2.583h-3.6c-2.263 0-3.394 0-4.097.703c-.703.703-.703 1.834-.703 4.097v3.6c0 2.263 0 3.394.703 4.097c.607.607 1.54.652 3.314.657",
                ],
                circles: []
            )
        case .check:
            // check.svg is drawn on a 16 grid; scaled to this one.
            return (paths: ["M5.25 12.75l4.5 4.5l9-10.5"], circles: [])
        case .shieldOut:
            return (
                paths: [
                    Self.shieldOutline,
                    "M9.5 14.5L14.5 9.5M10.5 9.5H14.5V13.5",
                ],
                circles: []
            )
        case .shieldKeyhole:
            return (
                paths: [
                    Self.shieldOutline,
                    "M12 13.5V16.5M13.4997 13.0976C12.0651 13.9259 10.2306 13.4344 9.40236 11.9997C8.57407 10.5651 9.06561 8.73065 10.5003 7.90236C11.085 7.56473 11.7363 7.4464 12.3587 7.52185C13.2632 7.63149 14.107 8.1504 14.5976 9.00025C15.0883 9.8501 15.1158 10.8403 14.7585 11.6785C14.5126 12.2552 14.0845 12.76 13.4997 13.0976Z",
                ],
                circles: []
            )
        case .shieldCross:
            return (
                paths: [
                    Self.shieldOutline,
                    "M14.5 9.5L9.50002 14.5M9.5 9.49998L14.5 14.5",
                ],
                circles: []
            )
        case .shieldCheck:
            return (
                paths: [
                    Self.shieldOutline,
                    "M9.5 12.4L10.9286 14L14.5 10",
                ],
                circles: []
            )
        case .dangerTriangle:
            return (
                paths: [
                    "M5.312 10.762C8.23 5.587 9.689 3 12 3s3.77 2.587 6.688 7.762l.364.644c2.425 4.3 3.638 6.45 2.542 8.022S17.786 21 12.364 21h-.728c-5.422 0-8.134 0-9.23-1.572s.117-3.722 2.542-8.022z",
                    "M12 8v5",
                ],
                circles: [(12, 16, 0.5)]
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
        return combined.applying(
            CGAffineTransform(scaleX: scale, y: scale)
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
            .stroke(
                color,
                style: StrokeStyle(
                    lineWidth: 1.5 * size / 24,
                    lineCap: .round, lineJoin: .round)
            )
            .frame(width: size, height: size)
    }
}
