import SwiftUI

/// A bounded decision card: quiet chrome, one prompt, optional context and a
/// persistent confirmation action. Selecting is never the same as sending.
struct QuestionPanel: View {
    let requestId: String
    let questions: [UserInputQuestion]
    var maximumHeight: CGFloat = 480
    var canRespond = true
    var stop: (() -> Void)?
    let respond: (String, [UserInputAnswer]) -> Void

    @State private var page = 0
    @State private var draft = QuestionAnswerDraft()
    @State private var expandedContext: Set<String> = []
    @State private var customAnswers: Set<String> = []
    @State private var contentHeight: CGFloat = 320
    @FocusState private var answerFocused: Bool

    var body: some View {
        if !questions.isEmpty {
            let question = questions[min(max(page, 0), questions.count - 1)]
            let presentation = QuestionPresentation(question)
            VStack(spacing: 0) {
                header(presentation)
                ScrollView {
                    content(question, presentation: presentation)
                        .padding(.horizontal, 16)
                        .padding(.bottom, 12)
                        .fixedSize(horizontal: false, vertical: true)
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: {
                            contentHeight = $0
                        }
                }
                .scrollBounceBehavior(.basedOnSize)
                .scrollDismissesKeyboard(.interactively)
                .frame(height: min(contentHeight, max(64, maximumHeight - 109)))
                .id(question.id)
                .accessibilityIdentifier("question-content")
                Rectangle().fill(Theme.border).frame(height: 1)
                footer(question, presentation: presentation)
            }
            .background(Theme.sheetPanel, in: RoundedRectangle(cornerRadius: 24))
            .overlay(RoundedRectangle(cornerRadius: 24).strokeBorder(Theme.border, lineWidth: 1))
            .padding(.horizontal, 12)
            .transition(.opacity)
        }
    }

    private func header(_ presentation: QuestionPresentation) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "bubble.left.and.text.bubble.right")
                .font(.system(size: 12))
                .accessibilityHidden(true)
            Text(presentation.header)
                .font(Theme.sans(11, weight: .medium))
                .lineLimit(1)
            Spacer(minLength: 8)
            if questions.count > 1 {
                Text("\(page + 1) of \(questions.count)")
                    .font(Theme.mono(10))
                    .fixedSize()
            }
            if let stop {
                Button(action: stop) {
                    Image(systemName: "stop.circle")
                        .font(.system(size: 16))
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(!canRespond)
                .accessibilityLabel("Stop task")
            }
        }
        .foregroundStyle(Theme.textMuted)
        .frame(height: 44)
        .padding(.leading, 16)
        .padding(.trailing, stop == nil ? 16 : 4)
    }

    private func content(_ question: UserInputQuestion,
                         presentation: QuestionPresentation) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(presentation.prompt)
                .font(Theme.sans(16, weight: .medium))
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("question-prompt")

            if let context = presentation.context, !context.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Button {
                        answerFocused = false
                        if !expandedContext.insert(question.id).inserted {
                            expandedContext.remove(question.id)
                        }
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "text.alignleft")
                                .font(.system(size: 11))
                            Text("Context").font(Theme.sans(12, weight: .medium))
                            Spacer()
                            Image(systemName: expandedContext.contains(question.id) ? "chevron.up" : "chevron.down")
                                .font(.system(size: 10, weight: .semibold))
                        }
                        .foregroundStyle(Theme.textMuted)
                        .frame(minHeight: 36)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("question-context-toggle")
                    .accessibilityValue(expandedContext.contains(question.id) ? "Expanded" : "Collapsed")
                    if expandedContext.contains(question.id) {
                        Text((try? AttributedString(markdown: context, options: .init(
                            interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(context))
                            .font(Theme.sans(12))
                            .foregroundStyle(Theme.textMuted)
                            .lineSpacing(3)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                            .accessibilityIdentifier("question-context")
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 4)
                .background(Theme.elementHover, in: RoundedRectangle(cornerRadius: 10))
            }

            if let selection = presentation.selection {
                VStack(alignment: .leading, spacing: 5) {
                    Label("Your selection", systemImage: "checkmark")
                        .font(Theme.sans(11, weight: .medium))
                    Text(selection)
                        .font(Theme.sans(13))
                        .foregroundStyle(Theme.text)
                }
                .foregroundStyle(Theme.textMuted)
            }

            if !question.options.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(question.multiSelect == true ? "Choose one or more" : "Choose one")
                        .font(Theme.sans(11))
                        .foregroundStyle(Theme.textFaint)
                        .padding(.bottom, 2)
                    ForEach(Array(question.options.enumerated()), id: \.offset) { index, option in
                        optionRow(option, index: index, question: question)
                    }
                }
            }

            if question.options.isEmpty || customAnswers.contains(question.id) {
                TextField(presentation.isOptionalComment ? "Add a comment (optional)" : "Your answer",
                          text: Binding(get: { draft.typed[question.id] ?? "" },
                                        set: { draft.typed[question.id] = $0 }),
                          axis: .vertical)
                    .font(Theme.sans(13))
                    .foregroundStyle(Theme.text)
                    .lineLimit(2...5)
                    .padding(12)
                    .background(Theme.elementHover, in: RoundedRectangle(cornerRadius: 12))
                    .overlay(RoundedRectangle(cornerRadius: 12)
                        .strokeBorder(answerFocused ? Theme.borderStrong : Theme.border, lineWidth: 1))
                    .focused($answerFocused)
                    .accessibilityIdentifier("question-answer")
            } else if !question.options.contains(QuestionPresentation.customAnswerOption) {
                // The dialog's custom-response sentinel is already an option;
                // don't show a second, competing freeform control beside it.
                Button {
                    draft.picked[question.id] = []
                    customAnswers.insert(question.id)
                    answerFocused = true
                } label: {
                    Label("Write an answer", systemImage: "square.and.pencil")
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textMuted)
                        .frame(minHeight: 44)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("question-custom-answer")
            }
        }
    }

    private func optionRow(_ option: String, index: Int, question: UserInputQuestion) -> some View {
        let selected = draft.picked[question.id, default: []].contains(option)
        let isCustom = option == QuestionPresentation.customAnswerOption
        return Button {
            answerFocused = false
            customAnswers.remove(question.id)
            draft.select(option, for: question)
        } label: {
            HStack(alignment: .center, spacing: 10) {
                Image(systemName: selected ? (question.multiSelect == true ? "checkmark.square.fill" : "checkmark.circle.fill")
                      : (question.multiSelect == true ? "square" : "circle"))
                    .font(.system(size: 17, weight: .regular))
                    .foregroundStyle(selected ? Theme.text : Theme.textFaint.opacity(0.6))
                    .frame(width: 20)
                Text(isCustom ? "Write a custom answer" : option)
                    .font(Theme.sans(13, weight: selected ? .medium : .regular))
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 11)
            .frame(minHeight: 44)
            .background(selected ? Theme.elementActive : Theme.elementHover, in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12)
                .strokeBorder(selected ? Theme.borderStrong : .clear, lineWidth: 1))
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
        .accessibilityIdentifier("question-option-\(index)")
    }

    private func footer(_ question: UserInputQuestion,
                        presentation: QuestionPresentation) -> some View {
        let answered = draft.hasAnswer(for: question)
        let canContinue = answered || presentation.isOptionalComment
        return HStack(spacing: 12) {
            if page > 0 {
                Button("Back") {
                    answerFocused = false
                    page -= 1
                }
                .font(Theme.sans(13, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(minHeight: 44)
            } else {
                Text(!canRespond ? "Reconnect to answer"
                     : presentation.isOptionalComment ? "Comment is optional" : "Confirm to send")
                    .font(Theme.sans(11))
                    .foregroundStyle(Theme.textFaint)
            }
            Spacer(minLength: 0)
            Button(page < questions.count - 1 ? "Next" : (presentation.isOptionalComment && !answered ? "Skip" : "Send answer")) {
                guard canContinue, canRespond else { return }
                answerFocused = false
                if page < questions.count - 1 {
                    page += 1
                } else {
                    respond(requestId, draft.answers(for: questions))
                }
            }
            .font(Theme.sans(13, weight: .semibold))
            .foregroundStyle(Theme.bg)
            .padding(.horizontal, 18)
            .frame(height: 44)
            .background(Theme.text.opacity(canContinue && canRespond ? 1 : 0.25), in: Capsule())
            .buttonStyle(.plain)
            .disabled(!canContinue || !canRespond)
            .accessibilityIdentifier("question-submit")
        }
        .frame(height: 44)
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
    }
}
