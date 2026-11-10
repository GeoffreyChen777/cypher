// The desktop's thirteen robot faces (crates/ui/assets/icons/bot*.svg),
// drawn natively from the same 24×24 geometry: a 1.5 stroke with round caps
// for the outline, and filled eyes. Kept as data so a face is one table row,
// as `BOT_FACES` is on the desktop.

import SwiftUI

enum BotFace: String, CaseIterable {
    case bot
    case botRound = "bot-round"
    case botVisor = "bot-visor"
    case botSquare = "bot-square"
    case botHappy = "bot-happy"
    case botCyclops = "bot-cyclops"
    case botHelmet = "bot-helmet"
    case botTv = "bot-tv"
    case botChip = "bot-chip"
    case botCat = "bot-cat"
    case botGhost = "bot-ghost"
    case botSleepy = "bot-sleepy"
    case botHeadset = "bot-headset"

    enum Element {
        case path(String)
        case rect(CGFloat, CGFloat, CGFloat, CGFloat, radius: CGFloat)
        case circle(CGFloat, CGFloat, radius: CGFloat)
    }

    /// (stroked, filled) elements in the 24×24 viewbox.
    var elements: (stroke: [Element], fill: [Element]) {
        switch self {
        case .bot:
            return (
                [
                    .rect(4, 8, 16, 12, radius: 4), .path("M12 8V5.25"), .circle(12, 3.75, radius: 1.25),
                    .path("M2 12.5v3M22 12.5v3M10 16.5h4"),
                ],
                [.circle(9, 12.75, radius: 1.25), .circle(15, 12.75, radius: 1.25)]
            )
        case .botRound:
            return (
                [
                    .circle(12, 13.5, radius: 7.5),
                    .path("M8.6 6.9L7.2 4M15.4 6.9L16.8 4M9.5 16.25c1.4 1.2 3.6 1.2 5 0"),
                ],
                [
                    .circle(6.75, 3, radius: 1), .circle(17.25, 3, radius: 1), .circle(9.5, 12.5, radius: 1.25),
                    .circle(14.5, 12.5, radius: 1.25),
                ]
            )
        case .botVisor:
            return (
                [
                    .rect(3.5, 7, 17, 13, radius: 4.5), .rect(6.5, 10.5, 11, 5, radius: 2.5),
                    .path("M12 7V4.75M10.5 18h3"),
                ],
                [.circle(12, 3.75, radius: 1), .circle(9.75, 13, radius: 1), .circle(14.25, 13, radius: 1)]
            )
        case .botSquare:
            return (
                [
                    .rect(5, 6, 14, 14, radius: 2.5), .rect(2.5, 10.5, 2.5, 5, radius: 1),
                    .rect(19, 10.5, 2.5, 5, radius: 1), .path("M12 6V3M10.5 3h3M9.5 15.5v2M12 15.5v2M14.5 15.5v2"),
                ],
                [.rect(8, 9.5, 2.75, 2.75, radius: 0.75), .rect(13.25, 9.5, 2.75, 2.75, radius: 0.75)]
            )
        case .botHappy:
            return (
                [
                    .rect(4, 7.5, 16, 12.5, radius: 5), .path("M12 7.5V5"), .circle(12, 3.75, radius: 1.25),
                    .path(
                        "M7.75 13.25c.6-1.25 2-1.25 2.5 0M13.75 13.25c.6-1.25 2-1.25 2.5 0M9.5 16.25c1.4 1.2 3.6 1.2 5 0"
                    ),
                ],
                []
            )
        case .botCyclops:
            return (
                [.rect(4, 7, 16, 13, radius: 5), .circle(12, 13, radius: 3), .path("M8.25 7L6.75 4M15.75 7l1.5-3")],
                [.circle(12, 13, radius: 1.25)]
            )
        case .botHelmet:
            return (
                [
                    .circle(12, 12, radius: 8.5), .rect(6.5, 8.5, 11, 6.5, radius: 3.25),
                    .path("M2 10.5v3M22 10.5v3M10.5 18h3"),
                ],
                [.circle(9.75, 11.75, radius: 1), .circle(14.25, 11.75, radius: 1)]
            )
        case .botTv:
            return (
                [.rect(3, 8, 18, 12, radius: 3), .path("M8.5 4L12 8l3.5-4M10.5 16.5h3M7 20v1.5M17 20v1.5")],
                [.circle(9, 13, radius: 1.25), .circle(15, 13, radius: 1.25)]
            )
        case .botChip:
            return (
                [
                    .rect(6, 6, 12, 12, radius: 2.5),
                    .path(
                        "M9 6V3.5M12 6V3.5M15 6V3.5M9 20.5V18M12 20.5V18M15 20.5V18M3.5 9H6M3.5 12H6M3.5 15H6M18 9h2.5M18 12h2.5M18 15h2.5M10 14c1.1.9 2.9.9 4 0"
                    ),
                ],
                [.circle(10, 10.75, radius: 1), .circle(14, 10.75, radius: 1)]
            )
        case .botCat:
            return (
                [
                    .path("M4.5 17V5l4.25 3.5h6.5L19.5 5v12a3 3 0 0 1-3 3h-9a3 3 0 0 1-3-3Z"),
                    .path("M11 16.25h2M2 13.25h2.5M2.25 16.25l2.25-.75M22 13.25h-2.5M21.75 16.25l-2.25-.75"),
                ],
                [.circle(9.25, 13, radius: 1.15), .circle(14.75, 13, radius: 1.15)]
            )
        case .botGhost:
            return (
                [
                    .path("M5 20.5V11a7 7 0 0 1 14 0v9.5l-2.33-1.5-2.34 1.5L12 19l-2.33 1.5L7.33 19 5 20.5Z"),
                    .path("M10.5 14.75h3"),
                ],
                [.circle(9.5, 11.25, radius: 1.25), .circle(14.5, 11.25, radius: 1.25)]
            )
        case .botSleepy:
            return (
                [
                    .rect(4, 8, 16, 12, radius: 4),
                    .path(
                        "M12 8V6.5c0-1.1.9-2 2-2h.5M7.75 12.75c.6 1.1 1.9 1.1 2.5 0M13.75 12.75c.6 1.1 1.9 1.1 2.5 0"),
                    .circle(12, 16.75, radius: 1),
                ],
                [.circle(15.75, 4.5, radius: 1)]
            )
        case .botHeadset:
            return (
                [
                    .rect(5.5, 7, 13, 13, radius: 4.5), .path("M3.75 11a8.25 8.25 0 0 1 16.5 0"),
                    .rect(2.5, 11, 2.5, 5, radius: 1.25), .rect(19, 11, 2.5, 5, radius: 1.25),
                    .path("M10.75 16.75h2.5"),
                ],
                [.circle(9.75, 12.75, radius: 1.15), .circle(14.25, 12.75, radius: 1.15)]
            )
        }
    }
}

/// One layer of a face (its outline or its eyes), scaled into the rect.
struct BotFaceShape: Shape {
    let face: BotFace
    let filled: Bool

    func path(in rect: CGRect) -> Path {
        var combined = Path()
        let elements = filled ? face.elements.fill : face.elements.stroke
        for element in elements {
            switch element {
            case .path(let data):
                combined.addPath(SVGPathParser.path(from: data))
            case .rect(let x, let y, let w, let h, let radius):
                combined.addRoundedRect(
                    in: CGRect(x: x, y: y, width: w, height: h), cornerSize: CGSize(width: radius, height: radius))
            case .circle(let cx, let cy, let radius):
                combined.addEllipse(in: CGRect(x: cx - radius, y: cy - radius, width: radius * 2, height: radius * 2))
            }
        }
        let scale = min(rect.width, rect.height) / 24
        let dx = rect.minX + (rect.width - 24 * scale) / 2
        let dy = rect.minY + (rect.height - 24 * scale) / 2
        return combined.applying(
            CGAffineTransform(scaleX: scale, y: scale).concatenating(CGAffineTransform(translationX: dx, y: dy)))
    }
}

/// A robot face at `size`, in `color`.
struct BotFaceView: View {
    let face: BotFace
    var size: CGFloat = 20
    var color: Color = Theme.text

    var body: some View {
        ZStack {
            BotFaceShape(face: face, filled: false)
                .stroke(color, style: StrokeStyle(lineWidth: 1.5 * size / 24, lineCap: .round, lineJoin: .round))
            BotFaceShape(face: face, filled: true).fill(color)
        }
        .frame(width: size, height: size)
    }
}

#Preview("Faces") {
    LazyVGrid(columns: Array(repeating: GridItem(.fixed(44)), count: 5), spacing: 16) {
        ForEach(BotFace.allCases, id: \.self) { face in
            BotFaceView(face: face, size: 32, color: BotStyle.color("indigo") ?? Theme.text)
        }
    }
    .padding()
    .background(Theme.surface)
}
