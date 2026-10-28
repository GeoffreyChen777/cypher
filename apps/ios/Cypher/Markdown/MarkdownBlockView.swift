// Markdown block rendering — metrics ported from crates/ui/src/markdown/render.rs.
//
// Every constant here mirrors the desktop values so the two apps read the same:
// body 14/22, headings (19/27, 16/24, 15/22, 14/22), code 12.5/18, block gap 12.
// Code blocks use a single selectable native text layout with fixed line
// metrics; syntax highlighting remains a paint-only recolor.

import SwiftUI

enum MD {
    static let textSize: CGFloat = 14
    static let lineHeight: CGFloat = 22
    static let blockGap: CGFloat = 12
    static let codeTextSize: CGFloat = 12.5
    static let codeLineHeight: CGFloat = 18
    static let codePaddingX: CGFloat = 12
    static let codePaddingY: CGFloat = 10
    static let inlineCodeRadius: CGFloat = 4.5

    static func headingMetrics(_ level: Int) -> (size: CGFloat, line: CGFloat) {
        switch level {
        case 1: return (19, 27)
        case 2: return (16, 24)
        case 3: return (15, 22)
        default: return (14, 22)
        }
    }
}

// MARK: - Block view

struct MarkdownBlockView: View {
    let block: MDBlock
    /// Identity for async highlight caching (row id).
    var cacheKey: String = ""
    /// The row is still streaming (an artifact's unclosed fence is still
    /// being written).
    var streaming = false

    var body: some View {
        switch block {
        case .paragraph(let runs):
            SelectableTranscriptText(attributed: TranscriptTextStyle.inline(runs))
                .frame(maxWidth: .infinity, alignment: .leading)

        case .heading(let level, let runs):
            let m = MD.headingMetrics(level)
            SelectableTranscriptText(
                attributed: TranscriptTextStyle.inline(
                    runs, size: m.size, weight: .semibold, lineHeight: m.line)
            )
            .frame(maxWidth: .infinity, alignment: .leading)

        case .codeBlock(let language, let code, let closed):
            if closed, isMermaidFence(language) {
                MermaidBlockView(code: code)
            } else if DiffFence.isDiff(language) {
                DiffFenceView(code: code)
            } else {
                CodeBlockView(language: language, code: code, cacheKey: cacheKey)
            }

        case .artifact(let kind, let title, let code, let closed):
            ArtifactCard(kind: kind, title: title, code: code, closed: closed, streaming: streaming)

        case .blockquote(let children):
            BlockquoteView(children: children, cacheKey: cacheKey)

        case .list(let orderedStart, let items):
            ListBlockView(orderedStart: orderedStart, items: items, cacheKey: cacheKey)

        case .table(let header, let rows, let align):
            TableBlockView(header: header, rows: rows, align: align)

        case .rule:
            Rectangle()
                .fill(Theme.border)
                .frame(height: 1)
        }
    }
}

// MARK: - Code block

struct CodeBlockView: View {
    let language: String?
    let code: String
    var cacheKey: String = ""

    /// An HTML or SVG fence's page, opened from its Preview button.
    @State private var page: ArtifactPage?

    var body: some View {
        Group {
            if let language, !language.isEmpty {
                FencePanel {
                    HStack(spacing: 8) {
                        Text(language)
                        Spacer(minLength: 0)
                        if let kind = ArtifactKind(language: language) {
                            Button {
                                page = ArtifactPage(
                                    kind: kind,
                                    title: ArtifactFence.displayTitle(kind: kind, title: nil, code: code),
                                    code: code)
                            } label: {
                                Text("Preview").font(Theme.sans(11, weight: .medium))
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("code-block-preview")
                        }
                    }
                } content: {
                    HighlightedCodeView(language: language, code: code)
                }
            } else {
                FencePanel(header: nil) {
                    HighlightedCodeView(language: language, code: code)
                }
            }
        }
        .fullScreenCover(item: $page) { ArtifactViewer(page: $0) }
    }
}

/// A fence's chrome: the rounded panel and, when given, the label strip above
/// its body (desktop `fence_frame`).
struct FencePanel<Header: View, Content: View>: View {
    let header: Header?
    @ViewBuilder let content: Content

    init(@ViewBuilder header: () -> Header, @ViewBuilder content: () -> Content) {
        self.header = header()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let header {
                header
                    .font(Theme.sans(11))
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 5)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(whiteAlpha(0.02))
                    .overlay(alignment: .bottom) {
                        Rectangle().fill(Theme.border).frame(height: 1)
                    }
            }
            content
        }
        .background(whiteAlpha(0.035))
        .clipShape(RoundedRectangle(cornerRadius: Theme.panelRadius))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.panelRadius)
                .strokeBorder(whiteAlpha(0.06), lineWidth: 1)
        )
    }
}

extension FencePanel where Header == EmptyView {
    init(header: Header?, @ViewBuilder content: () -> Content) {
        self.header = header
        self.content = content()
    }
}

/// A code block's body without its frame: selectable, scrolling sideways
/// rather than wrapping, highlighted off the main thread. Also the body of a
/// tool chip that shows code (a Script's).
struct HighlightedCodeView: View {
    let language: String?
    let code: String
    var size: CGFloat = MD.codeTextSize

    @State private var spans: [[TokenSpan]] = []

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            SelectableTranscriptText(
                attributed: TranscriptTextStyle.code(code, spans: spans, size: size),
                wraps: false
            )
            .padding(.horizontal, MD.codePaddingX)
            .padding(.vertical, MD.codePaddingY)
        }
        .task(id: code) {
            guard let lang = HighlightLanguage.forTag(language) else { return }
            let source = code
            let result = await Task.detached(priority: .utility) {
                Highlighter.highlight(code: source, language: lang)
            }.value
            guard !Task.isCancelled else { return }
            spans = result
        }
    }

}

// MARK: - Blockquote

struct BlockquoteView: View {
    let children: [MDBlock]
    var cacheKey: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(children.enumerated()), id: \.offset) { ix, child in
                MarkdownBlockView(block: child, cacheKey: "\(cacheKey)/q\(ix)")
                    .foregroundStyle(Theme.textMuted)
            }
        }
        .padding(.leading, 12)
        .padding(.trailing, 10)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            UnevenRoundedRectangle(
                topLeadingRadius: 0, bottomLeadingRadius: 0,
                bottomTrailingRadius: 6, topTrailingRadius: 6
            )
            .fill(Theme.accent.opacity(0.05))
        )
        .overlay(alignment: .leading) {
            Rectangle().fill(Theme.accent.opacity(0.6)).frame(width: 2)
        }
    }
}

// MARK: - Lists

struct ListBlockView: View {
    let orderedStart: Int?
    let items: [MDListItem]
    var cacheKey: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(items.enumerated()), id: \.offset) { ix, item in
                HStack(alignment: .top, spacing: 8) {
                    marker(ix: ix, item: item)
                        .frame(minWidth: 18, alignment: .trailing)
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(Array(item.children.enumerated()), id: \.offset) { cix, child in
                            MarkdownBlockView(block: child, cacheKey: "\(cacheKey)/l\(ix).\(cix)")
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func marker(ix: Int, item: MDListItem) -> some View {
        if let checked = item.checked {
            Image(systemName: checked ? "checkmark.square.fill" : "square")
                .font(.system(size: 12))
                .foregroundStyle(checked ? Theme.accent.opacity(0.85) : Theme.textMuted)
                .frame(height: MD.lineHeight)
        } else if let start = orderedStart {
            Text("\(start + ix).")
                .font(Theme.sans(MD.textSize))
                .foregroundStyle(Theme.accent.opacity(0.85))
                .frame(height: MD.lineHeight)
        } else {
            Circle()
                .fill(Theme.accent.opacity(0.85))
                .frame(width: 5, height: 5)
                .frame(height: MD.lineHeight)
        }
    }
}

// MARK: - Table

struct TableBlockView: View {
    let header: [[InlineRun]]
    let rows: [[[InlineRun]]]
    let align: [MDAlign]

    var body: some View {
        // Frameless: hairline rules only, no header fill, no outer radius
        // (desktop render.rs tables). Grid keeps columns content-proportional.
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { ix, cell in
                        cellView(cell, weight: .bold, column: ix)
                    }
                }
                divider
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { ix, cell in
                            cellView(cell, weight: .regular, column: ix)
                        }
                    }
                    divider
                }
            }
        }
    }

    private var divider: some View {
        Rectangle().fill(whiteAlpha(0.10)).frame(height: 1)
            .gridCellUnsizedAxes(.horizontal)
    }

    private func cellView(_ runs: [InlineRun], weight: Font.Weight, column: Int) -> some View {
        let alignment: Alignment =
            column < align.count
            ? (align[column] == .center ? .center : align[column] == .right ? .trailing : .leading)
            : .leading
        return SelectableTranscriptText(
            attributed: TranscriptTextStyle.inline(
                runs, weight: weight == .bold ? .bold : .regular), wraps: false, hugsContent: true
        )
        .padding(12)
        .frame(minWidth: 48, maxWidth: .infinity, alignment: alignment)
    }
}
