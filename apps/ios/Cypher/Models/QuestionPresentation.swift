import Foundation

/// Presentation only: Pi's dialog fallback puts the entire prompt in both
/// header and question. Keep the wire question/options untouched for replies.
struct QuestionPresentation {
    let header: String
    let prompt: String
    let context: String?
    let selection: String?
    let isOptionalComment: Bool

    init(_ question: UserInputQuestion) {
        let raw = question.question.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = question.header.trimmingCharacters(in: .whitespacesAndNewlines)
        var body = raw
        var selected: String?
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
        header = title.isEmpty || title == raw || title == prompt || title.contains("\n")
            ? "Your input" : title
        selection = selected
        isOptionalComment = title == "Optional comment" && selected != nil
    }

    static let customAnswerOption = "✏️ Type custom response..."
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

    func answers(for questions: [UserInputQuestion]) -> [UserInputAnswer] {
        questions.map { question in
            let text = text(for: question)
            let labels = text.isEmpty
                ? question.options.filter { picked[question.id, default: []].contains($0) }
                : [text]
            return UserInputAnswer(questionId: question.id, labels: labels)
        }
    }
}
