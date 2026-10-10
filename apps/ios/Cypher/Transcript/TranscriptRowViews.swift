// Transcript row views: user bubble, prose with veil, tool groups and chips,
// status glyphs, error and input chips (transcript.rs row renderers).

import SwiftUI

// MARK: - User bubble (transcript.rs:1671)

struct UserBubble: View {
    @Environment(\.commentDrafts) private var commentDrafts
    let text: String
    var pending = false
    var isSteer = false
    /// The chat's host device — where attachment files live (read-back key).
    var deviceId = ""

    var body: some View {
        // Attachment refs ride the message text (message-attachments.ts
        // transport); split them out and render thumbnails above the bubble,
        // exactly like the desktop's user rows.
        let parsed = parseUserMessageImages(text)
        VStack(alignment: .trailing, spacing: 4) {
            if isSteer {
                Label("Steer", systemImage: "arrow.turn.down.right")
                    .font(Theme.sans(11, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                    .padding(.trailing, 12)
                    // One element reading "Steer": the icon's own name ("Arrow
                    // Turning Down Then Right") is noise, and split elements
                    // both carried the identifier.
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Steer")
                    .accessibilityIdentifier("steer-label")
            }
            VStack(alignment: .trailing, spacing: 8) {
                if !parsed.attachments.isEmpty, !deviceId.isEmpty {
                    UserAttachmentsStrip(deviceId: deviceId, attachments: parsed.attachments)
                }
                if !parsed.text.isEmpty {
                    SelectableTranscriptText(
                        attributed: TranscriptTextStyle.inline(
                            Mentions.inlineRuns(parsed.text)), hugsContent: true
                    )
                    .environment(\.commentDrafts, pending ? nil : commentDrafts)
                    .padding(.horizontal, 16)
                    // Optical centering for the native text line box: move
                    // the text up 1pt while preserving the bubble's height.
                    .padding(.top, 9)
                    .padding(.bottom, 11)
                    .background(
                        Theme.userBubble.opacity(isSteer ? 0.55 : 1),
                        in: RoundedRectangle(cornerRadius: Theme.bubbleRadius)
                    )
                    .overlay {
                        RoundedRectangle(cornerRadius: Theme.bubbleRadius)
                            .strokeBorder(isSteer ? Theme.border : .clear, lineWidth: 1)
                            .allowsHitTesting(false)
                    }
                    .frame(maxWidth: TranscriptView.maxContentWidth * 0.8, alignment: .trailing)
                }
            }
        }
        .opacity(pending ? 0.65 : 1)
        .frame(maxWidth: .infinity, alignment: .trailing)
    }
}

// MARK: - Prose row with veil

/// A run of prose blocks as one selectable text, so a drag selection can
/// cross its paragraphs, headings and list items. While streaming, the
/// row's appended text fades in (paint only).
struct ProseRowView: View {
    let row: TranscriptRow
    let blocks: [MDBlock]
    let streaming: Bool
    let veils: VeilStore

    var body: some View {
        // Keep the native text view's identity when a live block settles,
        // so an active selection is not destroyed.
        TimelineView(.animation(paused: !streaming)) { _ in
            SelectableTranscriptText(attributed: text)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .onDisappear { veils.drop(row.id) }
    }

    private var text: NSAttributedString {
        var prose = TranscriptTextStyle.prose(blocks)
        if row.muted { prose = TranscriptTextStyle.muted(prose) }
        guard streaming else { return prose }
        let veil = veils.veil(for: row.id, seeded: false)
        let faded = NSMutableAttributedString(attributedString: prose)
        veil.noteLength(faded.string.count)
        TranscriptTextStyle.applyVeil(veil, to: faded)
        return faded
    }
}

// MARK: - Tool group (transcript.rs render_tool_group)

struct ToolGroupView: View {
    let tools: [ToolItem]
    let open: Bool
    /// Part of a work run: the run's toggle is the header, so only the
    /// chips show.
    var nested = false
    let userToggled: Bool
    /// Part ids of the chips whose detail is open.
    let openChips: Set<String>
    let toggleChip: (String) -> Void
    let toggle: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header stays quiet even on failure — chips carry the red.
            if !nested { header }

            if open {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(tools.enumerated()), id: \.offset) { _, tool in
                        ToolChipRow(tool: tool, open: openChips.contains(tool.id)) {
                            toggleChip(tool.id)
                        }
                    }
                }
                .padding(.top, nested ? 0 : 2)
            }
        }
    }

    private var header: some View {
        Button(action: toggle) {
            HStack(spacing: 8) {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .rotationEffect(.degrees(open ? 90 : 0))
                    .frame(width: 18, height: 18)
                    .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 5))
                Text(toolGroupSummary(tools))
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .frame(height: 26)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: 6))
    }
}

/// A thought inside a work run (transcript.rs `render_thought_chip`): a chip
/// like the tool calls around it — the bulb, "Thought" and the thought's
/// first line, a spinner while it streams. Tapping shows or hides its text
/// below.
struct ThoughtChipRow: View {
    let live: Bool
    let preview: String
    let open: Bool
    let toggle: () -> Void

    /// The thought's text lines up with the chips' icons (card inset 12 +
    /// header padding 8).
    static let textInset: CGFloat = 20
    private static let radius: CGFloat = 9

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 8) {
                Image(systemName: "lightbulb")
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.textMuted)
                    .frame(width: 18, height: 18)
                Text(live ? "Thinking…" : "Thought")
                    .font(Theme.sans(12, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                Text(preview)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.text.opacity(0.85))
                    .lineLimit(1)
                Spacer(minLength: 0)
                if live { ToolStatusIcon(status: .running) }
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textMuted.opacity(0.8))
                    .rotationEffect(.degrees(open ? 90 : 0))
                    .frame(width: 18, height: 18)
            }
            .padding(.horizontal, 8)
            .frame(height: 30)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: Self.radius))
        .background(whiteAlpha(0.03))
        .clipShape(RoundedRectangle(cornerRadius: Self.radius))
        .overlay(RoundedRectangle(cornerRadius: Self.radius).strokeBorder(whiteAlpha(0.05), lineWidth: 1))
        .padding(.leading, 12)
        .padding(.vertical, 4)
        .accessibilityValue(open ? "Expanded" : "Collapsed")
        .accessibilityIdentifier("thought-chip")
    }
}

/// The toggle over folded rows — a translation's original, a thought
/// (transcript.rs `render_fold_toggle`): a chevron tile and a quiet label,
/// styled like a tool group's header. Toggling refolds the rows, which shows
/// or hides the blocks below it.
struct FoldToggle: View {
    let open: Bool
    let closedLabel: String
    let openLabel: String
    let identifier: String
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 8) {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .rotationEffect(.degrees(open ? 90 : 0))
                    .frame(width: 18, height: 18)
                    .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 5))
                Text(open ? openLabel : closedLabel)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .frame(height: 26)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: 6))
        .accessibilityValue(open ? "Expanded" : "Collapsed")
        .accessibilityIdentifier(identifier)
    }
}

/// 38pt row containing a 30pt card (transcript.rs tool_chip). A Script's
/// card opens onto its code, like the desktop's expandable chip card; the
/// calls a script made hang off the script's icon on a guide rail.
struct ToolChipRow: View {
    let tool: ToolItem
    var open = false
    var toggle: () -> Void = {}

    /// transcript.rs NESTED_RAIL_INSET: one nesting level's indent, so each
    /// rail lands under its caller's icon and the card starts 12pt past it.
    static let nestedIndent: CGFloat = 30
    /// Past a few levels the indent stops helping and starts eating the row.
    static let maxIndentLevels = 3
    /// A depth-0 chip's icon center (card inset 12 + padding 8 + half the
    /// 18pt icon), less half the 1pt rail.
    private static let firstRailX: CGFloat = 28.5
    private static let radius: CGFloat = 9

    private var levels: Int { min(tool.depth, Self.maxIndentLevels) }

    var body: some View {
        let script = tool.call.script.flatMap(RenderToolCall.scriptBody)
        card(script: script)
            .padding(.leading, 12 + CGFloat(levels) * Self.nestedIndent)
            .padding(.vertical, 4)
            .overlay(alignment: .leading) { rails }
    }

    private func card(script: (code: String, truncatedBy: Int)?) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if script != nil {
                Button(action: toggle) { header(expandable: true) }
                    .buttonStyle(PressWashButtonStyle(cornerRadius: Self.radius))
                    .accessibilityHint(open ? "Hides the script" : "Shows the script")
                    .accessibilityIdentifier("script-chip")
            } else {
                header(expandable: false)
            }
            if open, let script {
                Rectangle().fill(whiteAlpha(0.05)).frame(height: 1)
                // transcript.rs detail_body: tool bodies run a size under
                // Markdown code (11.5 on 18pt lines).
                HighlightedCodeView(language: "javascript", code: script.code, size: 11.5)
                if script.truncatedBy > 0 {
                    Text("… \(script.truncatedBy) more lines")
                        .font(Theme.sans(11))
                        .foregroundStyle(Theme.textFaint)
                        .padding(.horizontal, MD.codePaddingX)
                        .padding(.bottom, MD.codePaddingY)
                }
            }
        }
        .background(whiteAlpha(0.03))
        .clipShape(RoundedRectangle(cornerRadius: Self.radius))
        .overlay(RoundedRectangle(cornerRadius: Self.radius).strokeBorder(whiteAlpha(0.05), lineWidth: 1))
    }

    private func header(expandable: Bool) -> some View {
        HStack(spacing: 8) {
            Image(systemName: tool.call.chipSymbol)
                .font(.system(size: 10))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 18, height: 18)
            Text(tool.call.chipLabel)
                .font(Theme.sans(12, weight: .medium))
                .foregroundStyle(tool.isError ? Theme.danger : Theme.textMuted)
            Text(tool.call.chipDetail)
                .font(Theme.sans(12))
                .foregroundStyle(tool.isError ? Theme.danger : Theme.text.opacity(0.85))
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
            ToolStatusIcon(status: tool.status)
            if expandable {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textMuted.opacity(0.8))
                    .rotationEffect(.degrees(open ? 90 : 0))
                    .frame(width: 18, height: 18)
            }
        }
        .padding(.horizontal, 8)
        .frame(height: 30)
        .contentShape(Rectangle())
    }

    /// One guide rail per nesting level, each the row's full height so
    /// consecutive nested chips draw one continuous line.
    private var rails: some View {
        ZStack(alignment: .leading) {
            ForEach(0..<levels, id: \.self) { level in
                Rectangle()
                    .fill(whiteAlpha(0.08))
                    .frame(width: 1)
                    .padding(.leading, Self.firstRailX + CGFloat(level) * Self.nestedIndent)
            }
        }
        .allowsHitTesting(false)
    }
}

/// transcript.rs `tool_status_icon`: a check once the call completed, a
/// cross when it failed, and an arc turning while it runs (held still under
/// Reduce Motion). The desktop's own glyphs (icons/check.svg, cross.svg,
/// spinner.svg), 12pt in the 18pt slot the chip's other icons use.
struct ToolStatusIcon: View {
    let status: ToolStatus

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// One spinner turn. Linear, so the arc never appears to stall.
    static let spinPeriod: TimeInterval = 0.9
    private static let size: CGFloat = 12

    var body: some View {
        glyph
            .frame(width: 18, height: 18)
            .accessibilityElement()
            .accessibilityLabel(label)
            .accessibilityIdentifier("tool-status")
    }

    @ViewBuilder
    private var glyph: some View {
        switch status {
        case .completed:
            stroked(StatusGlyph(data: "M3.5 8.5l3 3 6-7"), Theme.success)
        case .failed:
            stroked(StatusGlyph(data: "m4.5 4.5 7 7m0-7-7 7"), Theme.danger)
        case .running:
            TimelineView(.animation(paused: reduceMotion)) { timeline in
                let turns = reduceMotion ? 0 : timeline.date.timeIntervalSinceReferenceDate / Self.spinPeriod
                ZStack {
                    stroked(StatusGlyph(track: true), Theme.textMuted).opacity(0.25)
                    stroked(StatusGlyph(data: "M8 2.5a5.5 5.5 0 0 1 5.5 5.5"), Theme.textMuted)
                }
                .rotationEffect(.degrees(turns.truncatingRemainder(dividingBy: 1) * 360))
            }
        }
    }

    /// The icons' 1.6/16 stroke, round-capped.
    private func stroked(_ glyph: StatusGlyph, _ color: Color) -> some View {
        glyph
            .stroke(color, style: StrokeStyle(lineWidth: 1.6 * Self.size / 16, lineCap: .round, lineJoin: .round))
            .frame(width: Self.size, height: Self.size)
    }

    private var label: String {
        switch status {
        case .running: return "Running"
        case .completed: return "Completed"
        case .failed: return "Failed"
        }
    }
}

/// A glyph in the desktop icons' 16×16 viewbox, scaled to its frame.
struct StatusGlyph: Shape {
    var data: String?
    /// The spinner's track: the full circle its arc runs along.
    var track = false

    func path(in rect: CGRect) -> Path {
        var path = data.map { SVGPathParser.path(from: $0) } ?? Path()
        if track { path.addEllipse(in: CGRect(x: 2.5, y: 2.5, width: 11, height: 11)) }
        let scale = min(rect.width, rect.height) / 16
        let dx = rect.minX + (rect.width - 16 * scale) / 2
        let dy = rect.minY + (rect.height - 16 * scale) / 2
        return path.applying(
            CGAffineTransform(scaleX: scale, y: scale)
                .concatenating(CGAffineTransform(translationX: dx, y: dy)))
    }
}

// MARK: - Chips (transcript.rs ErrorChip / InputChip)

struct ErrorChipView: View {
    let message: String

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 10))
                .foregroundStyle(Theme.dangerSoft.opacity(0.8))
                .frame(width: 20, height: 20)
                .background(Theme.danger.opacity(0.12), in: RoundedRectangle(cornerRadius: 6))
            Text("Error")
                .font(Theme.sans(12, weight: .medium))
                .foregroundStyle(Theme.text)
            Text(message)
                .font(Theme.sans(12))
                .foregroundStyle(Theme.text.opacity(0.8))
                .lineLimit(1)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .frame(height: 34)
        .background(Theme.danger.opacity(0.05), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Theme.danger.opacity(0.16), lineWidth: 1))
    }
}

struct InputChipView: View {
    let header: String
    let resolved: Bool

    var body: some View {
        // Neutral throughout — resolution never recolors.
        HStack(spacing: 8) {
            Image(systemName: "bubble.left.and.text.bubble.right")
                .font(.system(size: 10))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 20, height: 20)
                .background(whiteAlpha(0.09), in: RoundedRectangle(cornerRadius: 6))
            Text("Question")
                .font(Theme.sans(12, weight: .medium))
                .foregroundStyle(Theme.text)
            Text(resolved ? (header == "Your input" ? "Answered" : header) : "Awaiting your answer…")
                .font(Theme.sans(12))
                .foregroundStyle(Theme.textMuted)
                .lineLimit(1)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .frame(height: 34)
        .background(whiteAlpha(0.045), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(whiteAlpha(0.08), lineWidth: 1))
    }
}
