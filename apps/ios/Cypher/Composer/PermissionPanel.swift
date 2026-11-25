// The permission card that stands in for the question panel while the agent
// waits on an approval (composer/permission_card.rs and question_card.rs
// `permission_body` / `permission_actions`): the facts as rows, the notes as
// small lines, a change or input in a code block, and the options as
// buttons weighted by what they grant. One tap answers.

import SwiftUI

/// composer/permission_card.rs `PermissionRequest`: a permission prompt read
/// as its question, `Label: value` facts, sentences and a `Change:` or
/// `Input:` block. The labels are a contract with the Runtime's permission
/// extension (`cypher-permissions/core.ts` `approvalTitle`).
struct PermissionRequest: Equatable {
    /// What a permission prompt from a Runtime that does not mark its
    /// prompts is recognised by.
    static let stopOption = "Deny and stop"

    enum FactKind: String {
        case path = "Path"
        case matches = "Matches"
        case domain = "Domain"
        case tool = "Tool"
        case command = "Command"
        /// What the agent says a command outside the sandbox is for.
        case purpose = "Purpose"
        /// A command pattern the approval would allow from now on.
        case pattern = "Pattern"
        case reason = "Reason"
        /// A sentence about the network (older Runtimes).
        case network = "Network"
        /// A sentence worth knowing before answering.
        case note = "Note"
        /// Why the reviewing model asked instead of approving.
        case review = "Review"

        /// Sentences (network, notes, the review) have no row label.
        var isSentence: Bool { self == .network || self == .note || self == .review }

        /// Paths, patterns, domains and commands read best in the code face.
        var monospace: Bool {
            switch self {
            case .path, .matches, .domain, .tool, .command, .pattern: return true
            default: return false
            }
        }
    }

    struct Fact: Equatable {
        var kind: FactKind
        var value: String
    }

    var question: String
    var facts: [Fact]
    /// `("Change", diff)` or `("Input", tool input)`.
    var block: (title: String, text: String)?
    /// A secret, Cypher's own data, or running outside the sandbox is at
    /// stake.
    var serious: Bool

    var rows: [Fact] { facts.filter { !$0.kind.isSentence } }
    var sentences: [Fact] { facts.filter(\.kind.isSentence) }

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.question == rhs.question && lhs.facts == rhs.facts && lhs.serious == rhs.serious
            && lhs.block?.title == rhs.block?.title && lhs.block?.text == rhs.block?.text
    }

    /// The prompt's question line, without its context (the transcript's
    /// row for a prompt it cannot file under a call).
    static func question(of question: UserInputQuestion) -> String {
        let prompt = question.question.isEmpty ? question.header : question.question
        let head = prompt.components(separatedBy: "\n\nContext:\n").first ?? prompt
        return head.split(whereSeparator: \.isNewline).joined(separator: " ")
    }

    /// nil unless `question` is a permission prompt: it says so (or offers
    /// "Deny and stop"). A marked prompt always reads as one; an unmarked
    /// one only when its context starts with a fact — otherwise it shows as
    /// an ordinary question.
    ///
    /// A value runs over several lines (a multi-line command, a reason with
    /// line breaks): the extension indents its lines after the first, and an
    /// older extension leaves them as they are — any line that is not a
    /// known `Label: value` continues the fact before it.
    init?(_ question: UserInputQuestion) {
        let permission = question.permission != nil || question.options.contains(Self.stopOption)
        guard permission, question.multiSelect != true else { return nil }
        let marked = question.permission != nil
        let prompt = (question.question.isEmpty ? question.header : question.question)
            .replacingOccurrences(of: "\r\n", with: "\n")
        let split = prompt.range(of: "\n\nContext:\n")
        let text = String(prompt[..<(split?.lowerBound ?? prompt.endIndex)])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let context = split.map { String(prompt[$0.upperBound...]) } ?? ""
        var facts: [Fact] = []
        var blockTitle: String?
        var blockLines: [String] = []
        for raw in context.components(separatedBy: "\n") {
            if blockTitle != nil {
                blockLines.append(raw)
                continue
            }
            // Indented: always the value before going on.
            let indented = raw.hasPrefix("  ")
            if !indented {
                let line = raw.trimmingCharacters(in: .whitespaces)
                if line == "Change:" || line == "Input:" {
                    blockTitle = String(line.dropLast())
                    continue
                }
                if let colon = raw.range(of: ": "),
                    let kind = FactKind(rawValue: String(raw[..<colon.lowerBound]))
                {
                    facts.append(
                        Fact(kind: kind, value: String(raw[colon.upperBound...]).trimmingCharacters(in: .whitespaces)))
                    continue
                }
            }
            let continued = indented ? String(raw.dropFirst(2)) : raw
            if !facts.isEmpty {
                facts[facts.count - 1].value += "\n" + Self.trimmingTrailingWhitespace(continued)
            } else if raw.trimmingCharacters(in: .whitespaces).isEmpty {
                continue
            } else if marked {
                // Text ahead of any fact: a sentence, on a prompt that says
                // it is a permission request.
                facts.append(Fact(kind: .note, value: raw.trimmingCharacters(in: .whitespaces)))
            } else {
                return nil
            }
        }
        for index in facts.indices {
            facts[index].value = Self.trimmingTrailingWhitespace(facts[index].value)
        }
        guard marked || !facts.isEmpty else { return nil }
        self.question = text
        self.facts = facts
        self.block = blockTitle.map { ($0, blockLines.joined(separator: "\n").trimmingCharacters(in: .newlines)) }
        self.serious =
            text.contains("sensitive") || text.contains("Cypher's own") || text.contains("outside the sandbox")
    }

    private static func trimmingTrailingWhitespace(_ text: String) -> String {
        var text = Substring(text)
        while let last = text.last, last.isWhitespace { text = text.dropLast() }
        return String(text)
    }

    /// permission_card.rs `OptionRole`: how much an option weighs on the
    /// card. The first approval is the narrowest: the extension lists it
    /// first.
    enum OptionRole: Equatable {
        case primary, secondary, deny, stop
    }

    static func roles(_ options: [String]) -> [OptionRole] {
        var primaryTaken = false
        return options.map { option in
            switch option {
            case stopOption: return .stop
            case "Deny": return .deny
            default:
                if primaryTaken { return .secondary }
                primaryTaken = true
                return .primary
            }
        }
    }

    /// How long a broader approval lasts, read off the extension's labels
    /// (`core.ts` `HUMAN`, `TASK`, `DOMAIN_ONCE`, `ALWAYS_IN`).
    enum Grant: Equatable {
        /// This command, this task, this chat.
        case scoped
        /// Always, everywhere or in a project: kept as a rule.
        case lasting

        init(_ label: String) {
            self = label.hasPrefix("Always") ? .lasting : .scoped
        }
    }

    /// One option as a card shows it: the narrowest approval as the
    /// prominent capsule, every other option as a round glass icon button
    /// with a short name under it — none folded away.
    struct ShownOption: Equatable {
        /// Its place in the prompt's options (`permission-option-<index>`).
        let index: Int
        /// The label sent back, and what VoiceOver reads.
        let label: String
        let role: OptionRole
        let symbol: String
        /// The short name under the icon.
        let caption: String

        var prominent: Bool { role == .primary }
        /// A refusal: a destructive button, painted red.
        var destructive: Bool { role == .deny || role == .stop }

        init(index: Int, label: String, role: OptionRole) {
            self.index = index
            self.label = label
            self.role = role
            switch role {
            case .primary:
                symbol = "checkmark"
                caption = label
            case .secondary:
                symbol = Grant(label) == .lasting ? "checkmark.shield.fill" : "checkmark.shield"
                caption = Self.caption(label)
            case .deny:
                symbol = "xmark"
                caption = label
            case .stop:
                symbol = "stop.fill"
                caption = "Stop"
            }
        }

        /// A broader approval's name without what every approval says:
        /// "Allow for this chat" is "This chat", "Always allow" "Always".
        private static func caption(_ label: String) -> String {
            for prefix in ["Allow for ", "Allow "] where label.hasPrefix(prefix) && label.count > prefix.count {
                let rest = label.dropFirst(prefix.count)
                return rest.prefix(1).uppercased() + rest.dropFirst()
            }
            if label.hasSuffix(" allow") { return String(label.dropLast(" allow".count)) }
            return label
        }
    }

    /// A card's options in the order it sets them, leading to trailing: the
    /// prompt's own order read from the far end — Deny and stop, Deny, the
    /// broadest approval … the narrowest — so the primary approval comes
    /// last, the refusals lead and the broadest grant sits away from the
    /// primary. Every option is there.
    static func shownOptions(_ options: [String]) -> [ShownOption] {
        let roles = roles(options)
        let shown = options.enumerated().reversed().map {
            ShownOption(index: $0.offset, label: $0.element, role: roles[$0.offset])
        }
        return shown.filter { !$0.prominent } + shown.filter(\.prominent)
    }
}

/// The card: a shield header, the question, the facts, then the options.
/// Embedded in a card of its own (a Needs you item's), it draws no frame,
/// header or question (the card's head and title say them): the facts on
/// plates concentric with the card, then every option on screen as system
/// glass buttons (`PermissionOptionRow`; needs.rs `asking_content`).
struct PermissionPanel: View {
    enum Style {
        /// Over the composer: its own card.
        case panel
        /// Inside a `NeedsYouCard`.
        case embedded
    }

    let requestId: String
    let question: UserInputQuestion
    let request: PermissionRequest
    var maximumHeight: CGFloat = 480
    var canRespond = true
    /// Why this request is back after an answer.
    var notice: String?
    var style = Style.panel
    let respond: (String, [UserInputAnswer]) -> Void

    @Environment(\.dynamicTypeSize) private var typeSize
    @State private var contentHeight: CGFloat = 240
    @State private var answered = false

    var body: some View {
        switch style {
        case .panel: panel
        case .embedded: embedded
        }
    }

    private var embedded: some View {
        VStack(alignment: .leading, spacing: 0) {
            facts
            NeedsCardActions {
                VStack(alignment: .leading, spacing: 8) {
                    PermissionOptionRow(
                        options: question.options, enabled: canRespond && !answered, choose: choose)
                    if let note = footnote {
                        Text(note.text)
                            .font(Theme.sans(12, relativeTo: .caption))
                            .foregroundStyle(note.color)
                            .frame(maxWidth: .infinity, alignment: .trailing)
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("permission-panel")
    }

    private var footnote: (text: String, color: Color)? {
        if let notice, canRespond { return (notice, Theme.danger) }
        if !canRespond { return ("Reconnect to answer", Theme.textFaint) }
        return nil
    }

    private func choose(_ option: String) {
        guard canRespond, !answered else { return }
        answered = true
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        respond(requestId, [UserInputAnswer(questionId: question.id, labels: [option])])
    }

    /// A size in the panel's type: fixed over the composer, following
    /// Dynamic Type in a card.
    private func sans(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        Theme.sans(size, weight: weight, relativeTo: style == .embedded ? .body : nil)
    }

    private var panel: some View {
        VStack(spacing: 0) {
            header
            Rectangle().fill(Theme.border).frame(height: 1)
            ScrollView {
                content
                    .padding(.horizontal, 16)
                    .padding(.vertical, 14)
                    .fixedSize(horizontal: false, vertical: true)
                    .onGeometryChange(for: CGFloat.self) {
                        $0.size.height
                    } action: {
                        contentHeight = $0
                    }
            }
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: min(contentHeight, max(64, maximumHeight - 120 - CGFloat(question.options.count) * 50)))
            .accessibilityIdentifier("permission-content")
            Rectangle().fill(Theme.border).frame(height: 1)
            actions
        }
        .background(Theme.sheetPanel, in: RoundedRectangle(cornerRadius: 24))
        .overlay(RoundedRectangle(cornerRadius: 24).strokeBorder(Theme.border, lineWidth: 1))
        .padding(.horizontal, 12)
        .transition(.opacity)
        .accessibilityIdentifier("permission-panel")
    }

    private var header: some View {
        HStack(spacing: 10) {
            LineIconView(.shieldCheck, size: 15, color: request.serious ? Theme.warning : Theme.textMuted)
                .accessibilityHidden(true)
            Text(question.askedBy.map { "Permission request from \($0)" } ?? "Permission request")
                .font(Theme.sans(13, weight: .medium))
                .foregroundStyle(Theme.textMuted)
            Spacer(minLength: 8)
        }
        .frame(height: 48)
        .padding(.horizontal, 16)
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(request.question)
                .font(Theme.sans(16, weight: .medium))
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("permission-question")
            facts
        }
    }

    /// What it asks about: the facts as rows on a wash, the notes, the
    /// change or input.
    private var facts: some View {
        VStack(alignment: .leading, spacing: 12) {
            if !request.rows.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(request.rows.enumerated()), id: \.offset) { _, fact in
                        factRow(fact)
                    }
                }
                .plate(style, padding: 12, panelRadius: 12, panelFill: Theme.elementHover)
            }
            ForEach(Array(request.sentences.enumerated()), id: \.offset) { _, fact in
                Text(fact.value)
                    .font(sans(12))
                    .foregroundStyle(fact.kind == .review ? Theme.warning : Theme.textMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let block = request.block, !block.text.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(block.title)
                        .font(sans(11, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                    Text(block.text)
                        .font(Theme.mono(11.5))
                        .foregroundStyle(Theme.text.opacity(0.9))
                        .textSelection(.enabled)
                        .lineLimit(24)
                        .plate(style, padding: 10, panelRadius: Theme.panelRadius, panelFill: whiteAlpha(0.04))
                }
            }
        }
    }

    private func factRow(_ fact: PermissionRequest.Fact) -> some View {
        // At an accessibility text size in a card, the label goes over its
        // value: a 62pt column can't hold it.
        let layout =
            style == .embedded && typeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 2))
            : AnyLayout(HStackLayout(alignment: .firstTextBaseline, spacing: 10))
        return layout {
            Text(fact.kind.rawValue)
                .font(sans(11, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: style == .embedded && typeSize.isAccessibilitySize ? nil : 62, alignment: .leading)
            Text(fact.value)
                .font(fact.kind.monospace ? Theme.mono(12) : sans(13))
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .combine)
    }

    private var actions: some View {
        let roles = PermissionRequest.roles(question.options)
        return VStack(spacing: 8) {
            ForEach(Array(question.options.enumerated()), id: \.offset) { index, option in
                optionButton(option, role: roles[index], index: index)
            }
            if let notice, canRespond {
                Text(notice)
                    .font(Theme.sans(11))
                    .foregroundStyle(Theme.danger)
                    .lineLimit(2)
            } else if !canRespond {
                Text("Reconnect to answer")
                    .font(Theme.sans(11))
                    .foregroundStyle(Theme.textFaint)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    /// The narrowest approval filled, broader ones outlined, Deny quiet,
    /// Deny and stop in red.
    private func optionButton(_ option: String, role: PermissionRequest.OptionRole, index: Int) -> some View {
        Button {
            guard canRespond, !answered else { return }
            answered = true
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            respond(requestId, [UserInputAnswer(questionId: question.id, labels: [option])])
        } label: {
            Text(option)
                .font(Theme.sans(14, weight: role == .primary ? .semibold : .medium))
                .foregroundStyle(foreground(role))
                .frame(maxWidth: .infinity, minHeight: 44)
                .background(background(role), in: Capsule())
                .overlay(Capsule().strokeBorder(role == .secondary ? Theme.borderStrong : .clear, lineWidth: 1))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(!canRespond || answered)
        .opacity(canRespond ? 1 : 0.45)
        .accessibilityIdentifier("permission-option-\(index)")
    }

    private func foreground(_ role: PermissionRequest.OptionRole) -> Color {
        switch role {
        case .primary: return Theme.bg
        case .secondary, .deny: return Theme.text
        case .stop: return Theme.danger.opacity(0.9)
        }
    }

    private func background(_ role: PermissionRequest.OptionRole) -> Color {
        switch role {
        case .primary: return Theme.text
        case .secondary: return .clear
        case .deny: return Theme.elementHover
        case .stop: return .clear
        }
    }
}

extension View {
    /// A plate of the permission card: over the composer as it always was;
    /// in a Needs you card, the card's own plate (concentric, continuous).
    @ViewBuilder
    fileprivate func plate(
        _ style: PermissionPanel.Style, padding: CGFloat, panelRadius: CGFloat, panelFill: Color
    ) -> some View {
        switch style {
        case .panel:
            self.padding(padding)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(panelFill, in: RoundedRectangle(cornerRadius: panelRadius))
        case .embedded:
            self.needsCardPlate(padding: padding)
        }
    }
}

/// A permission prompt's options in a card, all of them on screen: the
/// refusals and the broader approvals as round glass icon buttons, each
/// named under it, and the narrowest approval last as the prominent capsule
/// filling the rest of the line (`PermissionRequest.shownOptions`). Where
/// one line can't hold them (a narrow phone, a large text size), the icons
/// spread evenly across the card, on as many lines as they need, and the
/// capsule takes the line under them — nothing folds into a menu, nothing
/// clips.
struct PermissionOptionRow: View {
    let options: [String]
    var enabled = true
    let choose: (String) -> Void

    var body: some View {
        let shown = PermissionRequest.shownOptions(options)
        let icons = shown.filter { !$0.prominent }
        let primary = shown.filter(\.prominent)
        ViewThatFits(in: .horizontal) {
            // The capsule level with the circles: the row lines up on the
            // controls' centres, the names hang under the circles.
            HStack(alignment: .decisionCentre, spacing: Theme.iconButtonGap) {
                ForEach(icons, id: \.index) { icon($0) }
                ForEach(primary, id: \.index) { capsule($0) }
            }
            VStack(alignment: .leading, spacing: NeedsCardMetrics.buttonGap) {
                IconOptionGrid {
                    ForEach(icons, id: \.index) { icon($0) }
                }
                ForEach(primary, id: \.index) { capsule($0) }
            }
        }
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
    }

    private func icon(_ option: PermissionRequest.ShownOption) -> some View {
        GlassIconOption(
            symbol: option.symbol, label: option.label, caption: option.caption,
            tone: option.destructive ? .destructive : .ink, identifier: "permission-option-\(option.index)"
        ) {
            choose(option.label)
        }
    }

    /// The narrowest approval: a prominent glass capsule, the decision
    /// buttons' one height, as wide as the line leaves it.
    private func capsule(_ option: PermissionRequest.ShownOption) -> some View {
        NeedsCardButton(title: option.label, kind: .primary, identifier: "permission-option-\(option.index)") {
            choose(option.label)
        }
    }
}
