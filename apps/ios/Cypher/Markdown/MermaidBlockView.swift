// A closed ```mermaid fence: its diagram once drawn (or its source, when the
// reader switched to it), the code with Mermaid's complaint when it cannot be
// drawn, the plain code until then (desktop markdown/mermaid.rs `render`).

import SwiftUI

struct MermaidBlockView: View {
    let code: String

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.displayScale) private var displayScale
    @State private var outcome: MermaidOutcome?

    /// Air around a diagram inside its panel.
    static let padding: CGFloat = 16
    /// A diagram wider than the column shrinks to fit down to this share of
    /// its natural size; past that it keeps the size and scrolls sideways.
    static let minFit: CGFloat = 0.6

    private var key: MermaidRenderer.Key {
        MermaidRenderer.Key(
            source: code, palette: .of(dark: colorScheme == .dark), scale: displayScale)
    }

    var body: some View {
        let toggles = MermaidSourceToggles.shared
        // A settled drawing shows on the first frame (a row scrolled back in
        // must not flash its source); the task fills in everything else.
        let current = outcome ?? MermaidRenderer.shared.cached(key)
        Group {
            switch current {
            case .image(let image, let size) where !toggles.showsSource(code):
                FencePanel {
                    header(source: false)
                } content: {
                    diagram(image, size: size)
                }
            case .image:
                FencePanel {
                    header(source: true)
                } content: {
                    HighlightedCodeView(language: "mermaid", code: code)
                        .accessibilityIdentifier("mermaid-source")
                }
            case .failed(let error):
                FencePanel {
                    HStack(spacing: 8) {
                        Text("mermaid")
                        Text(error)
                            .foregroundStyle(Theme.dangerSoft)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                    .accessibilityElement(children: .combine)
                } content: {
                    HighlightedCodeView(language: "mermaid", code: code)
                }
            case nil:
                CodeBlockView(language: "mermaid", code: code)
            }
        }
        .task(id: key) {
            let key = key
            if MermaidRenderer.shared.isSettled(key) {
                outcome = MermaidRenderer.shared.cached(key)
                return
            }
            let drawn = await MermaidRenderer.shared.render(key)
            guard !Task.isCancelled else { return }
            outcome = drawn
        }
    }

    private func header(source: Bool) -> some View {
        HStack(spacing: 10) {
            Text("mermaid")
            ViewSwitch(source: source) { showSource in
                MermaidSourceToggles.shared.set(code, showsSource: showSource)
            }
        }
    }

    /// Fits the column between `minFit` and natural size, centered; wider
    /// than that it scrolls sideways at `minFit`.
    private func diagram(_ image: UIImage, size: CGSize) -> some View {
        let ratio = size.width / size.height
        let picture = Image(uiImage: image)
            .resizable()
            .interpolation(.high)
            .accessibilityLabel("Mermaid diagram")
            .accessibilityIdentifier("mermaid-diagram")
        return ViewThatFits(in: .horizontal) {
            picture
                .aspectRatio(ratio, contentMode: .fit)
                .frame(
                    minWidth: size.width * Self.minFit, idealWidth: size.width * Self.minFit,
                    maxWidth: size.width
                )
                .padding(Self.padding)
                .frame(maxWidth: .infinity)
            ScrollView(.horizontal, showsIndicators: false) {
                picture
                    .frame(
                        width: size.width * Self.minFit, height: size.width * Self.minFit / ratio
                    )
                    .padding(Self.padding)
            }
        }
    }
}

/// Two icon segments, Diagram and Source; the inactive one switches.
private struct ViewSwitch: View {
    let source: Bool
    let onSwitch: (Bool) -> Void

    var body: some View {
        HStack(spacing: 1) {
            segment("point.3.connected.trianglepath.dotted", label: "Diagram", active: !source) {
                onSwitch(false)
            }
            segment("chevron.left.forwardslash.chevron.right", label: "Source", active: source) {
                onSwitch(true)
            }
        }
        .padding(1)
        .background(whiteAlpha(0.035), in: RoundedRectangle(cornerRadius: 5))
    }

    private func segment(_ symbol: String, label: String, active: Bool, action: @escaping () -> Void)
        -> some View
    {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(active ? Theme.textMuted : Theme.textMuted.opacity(0.55))
                .frame(width: 26, height: 16)
                .background(active ? whiteAlpha(0.10) : .clear, in: RoundedRectangle(cornerRadius: 4))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .allowsHitTesting(!active)
        .accessibilityLabel(label)
        .accessibilityIdentifier("mermaid-show-\(label.lowercased())")
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}
