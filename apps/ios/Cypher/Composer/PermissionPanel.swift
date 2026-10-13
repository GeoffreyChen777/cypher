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

    /// nil unless `question` is a permission prompt that reads as one: it
    /// says so (or offers "Deny and stop"), and every context line is a
    /// known fact — otherwise it shows as an ordinary question.
    init?(_ question: UserInputQuestion) {
        let permission = question.permission != nil || question.options.contains(Self.stopOption)
        guard permission, question.multiSelect != true else { return nil }
        let prompt = question.question.isEmpty ? question.header : question.question
        guard let split = prompt.range(of: "\n\nContext:\n") else { return nil }
        let text = String(prompt[..<split.lowerBound])
        var facts: [Fact] = []
        var blockTitle: String?
        var blockLines: [String] = []
        for raw in prompt[split.upperBound...].components(separatedBy: "\n") {
            if blockTitle != nil {
                blockLines.append(raw)
                continue
            }
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line == "Change:" || line == "Input:" {
                blockTitle = String(line.dropLast())
                continue
            }
            if line.isEmpty { continue }
            guard let colon = raw.range(of: ": "),
                let kind = FactKind(rawValue: String(raw[..<colon.lowerBound]))
            else { return nil }
            facts.append(Fact(kind: kind, value: String(raw[colon.upperBound...]).trimmingCharacters(in: .whitespaces)))
        }
        guard !facts.isEmpty else { return nil }
        self.question = text
        self.facts = facts
        self.block = blockTitle.map { ($0, blockLines.joined(separator: "\n").trimmingCharacters(in: .newlines)) }
        self.serious =
            text.contains("sensitive") || text.contains("Cypher's own") || text.contains("outside the sandbox")
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
}

/// The card: a shield header, the question, the facts, then the options.
struct PermissionPanel: View {
    let requestId: String
    let question: UserInputQuestion
    let request: PermissionRequest
    var maximumHeight: CGFloat = 480
    var canRespond = true
    /// Why this request is back after an answer.
    var notice: String?
    let respond: (String, [UserInputAnswer]) -> Void

    @State private var contentHeight: CGFloat = 240
    @State private var answered = false

    var body: some View {
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
            Text("Permission request")
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
            if !request.rows.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(request.rows.enumerated()), id: \.offset) { _, fact in
                        factRow(fact)
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.elementHover, in: RoundedRectangle(cornerRadius: 12))
            }
            ForEach(Array(request.sentences.enumerated()), id: \.offset) { _, fact in
                Text(fact.value)
                    .font(Theme.sans(12))
                    .foregroundStyle(fact.kind == .review ? Theme.warning : Theme.textMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let block = request.block, !block.text.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(block.title)
                        .font(Theme.sans(11, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                    Text(block.text)
                        .font(Theme.mono(11.5))
                        .foregroundStyle(Theme.text.opacity(0.9))
                        .textSelection(.enabled)
                        .lineLimit(24)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(whiteAlpha(0.04), in: RoundedRectangle(cornerRadius: 10))
                }
            }
        }
    }

    private func factRow(_ fact: PermissionRequest.Fact) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(fact.kind.rawValue)
                .font(Theme.sans(11, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 62, alignment: .leading)
            Text(fact.value)
                .font(fact.kind.monospace ? Theme.mono(12) : Theme.sans(13))
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
