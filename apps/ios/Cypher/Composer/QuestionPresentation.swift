// Display model for QuestionPanel: splits Pi's dialog prompt into header,
// context and selection, and holds the in-progress answer draft.

import Foundation

/// Presentation only: Pi's dialog fallback puts the entire prompt in both
/// header and question. Keep the wire question/options untouched for replies.
struct QuestionPresentation {
    let header: String
    let prompt: String
    let context: String?
    let selection: String?
    let isOptionalComment: Bool
    /// pi-ask-user's RPC multi-select arrives as a free-text prompt listing
    /// numbered options to type back comma-separated. Recovered as real
    /// checkboxes; the answer goes back as that one joined string.
    let listedOptions: [ListedOption]

    struct ListedOption: Equatable {
        let title: String
        let description: String?
    }

    init(_ question: UserInputQuestion) {
        let raw = question.question.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = question.header.trimmingCharacters(in: .whitespacesAndNewlines)
        var body = raw
        var selected: String?
        var listed: [ListedOption] = []
        if question.options.isEmpty, question.multiSelect != true, title != "Optional comment",
            let range = body.range(of: Self.listedOptionsMarker),
            let parsed = Self.parseListedOptions(String(body[range.upperBound...]))
        {
            listed = parsed
            body = String(body[..<range.lowerBound])
        }
        // Only the known comment stage owns this delimiter. Ordinary context
        // may quote these words, and must not be silently reclassified.
        if title == "Optional comment" {
            for marker in ["\n\nSelected option:\n", "\n\nSelected options:\n"] {
                if let range = body.range(of: marker, options: .backwards) {
                    selected = String(body[range.upperBound...])
                    body = String(body[..<range.lowerBound])
                    break
                }
            }
        }
        if let range = body.range(of: "\n\nContext:\n") {
            prompt = String(body[..<range.lowerBound])
            context = String(body[range.upperBound...])
        } else {
            prompt = body
            context = nil
        }
        header =
            title.isEmpty || title == raw || title == prompt || title.contains("\n")
            ? "Your input" : title
        selection = selected
        isOptionalComment = title == "Optional comment" && selected != nil
        listedOptions = listed
    }

    static let customAnswerOption = "✏️ Type custom response..."
    static let listedOptionsMarker = "\n\nOptions (select one or more):\n"

    /// `1. title — description` rows; lines that don't start the next number
    /// continue the previous description. Anything else isn't the list.
    static func parseListedOptions(_ list: String) -> [ListedOption]? {
        var rows: [(title: String, description: String?)] = []
        for line in list.replacingOccurrences(of: "\r\n", with: "\n").split(
            separator: "\n", omittingEmptySubsequences: false)
        {
            let prefix = "\(rows.count + 1). "
            if line.hasPrefix(prefix) {
                let row = line.dropFirst(prefix.count)
                if let dash = row.range(of: " — ") {
                    rows.append(
                        (
                            String(row[..<dash.lowerBound]).trimmingCharacters(in: .whitespaces),
                            String(row[dash.upperBound...]).trimmingCharacters(in: .whitespaces)
                        ))
                } else {
                    rows.append((row.trimmingCharacters(in: .whitespaces), nil))
                }
            } else if !rows.isEmpty {
                let text = line.trimmingCharacters(in: .whitespaces)
                guard !text.isEmpty else { continue }
                let previous = rows[rows.count - 1].description
                rows[rows.count - 1].description = previous.map { "\($0)\n\(text)" } ?? text
            } else if !line.trimmingCharacters(in: .whitespaces).isEmpty {
                return nil
            }
        }
        guard !rows.isEmpty, !rows.contains(where: { $0.title.isEmpty }) else { return nil }
        return rows.map { ListedOption(title: $0.title, description: $0.description) }
    }

    /// The question as the panel renders it: listed options become a real
    /// multi-select. The wire question is still what answers are keyed on.
    func displayQuestion(_ wire: UserInputQuestion) -> UserInputQuestion {
        guard !listedOptions.isEmpty else { return wire }
        var question = wire
        question.options = listedOptions.map(\.title)
        question.multiSelect = true
        return question
    }
}

/// Answer order follows the original options, not Set iteration. Whitespace
/// alone is not an answer, and choosing an option clears any custom draft.
struct QuestionAnswerDraft {
    var picked: [String: Set<String>] = [:]
    var typed: [String: String] = [:]

    mutating func select(_ option: String, for question: UserInputQuestion) {
        typed[question.id] = nil
        if question.multiSelect == true {
            if picked[question.id, default: []].contains(option) {
                picked[question.id]?.remove(option)
            } else {
                picked[question.id, default: []].insert(option)
            }
        } else {
            picked[question.id] = [option]
        }
    }

    func hasAnswer(for question: UserInputQuestion) -> Bool {
        !text(for: question).isEmpty || !picked[question.id, default: []].isEmpty
    }

    func text(for question: UserInputQuestion) -> String {
        (typed[question.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Answers for the wire questions (not their display forms).
    func answers(for questions: [UserInputQuestion]) -> [UserInputAnswer] {
        questions.map { question in
            let text = text(for: question)
            guard text.isEmpty else { return UserInputAnswer(questionId: question.id, labels: [text]) }
            let presentation = QuestionPresentation(question)
            let picks = presentation.displayQuestion(question).options.filter {
                picked[question.id, default: []].contains($0)
            }
            let labels: [String]
            if !presentation.listedOptions.isEmpty {
                labels = picks.isEmpty ? [] : [picks.joined(separator: ", ")]
            } else if picks.isEmpty && presentation.isOptionalComment {
                // A blank optional comment is an answer, not a dismissal: no
                // labels at all reach pi-ask-user as a cancel, which throws
                // away the option picked in the stage before.
                labels = [""]
            } else {
                labels = picks
            }
            return UserInputAnswer(questionId: question.id, labels: labels)
        }
    }
}
