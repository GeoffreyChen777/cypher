import SwiftUI
import UIKit

extension EnvironmentValues {
    @Entry var commentDrafts: CommentDrafts? = nil
}

struct PendingCommentsBar: View {
    let drafts: CommentDrafts
    var body: some View {
        if !drafts.comments.isEmpty {
            Button { drafts.showList() } label: {
                HStack(spacing: 6) {
                    Image(systemName: "text.bubble").font(.system(size: 12))
                    Text("\(drafts.comments.count) \(drafts.comments.count == 1 ? "comment" : "comments") pending")
                    Image(systemName: "chevron.up").font(.system(size: 8, weight: .semibold))
                    Spacer(minLength: 0)
                }
                .font(Theme.sans(12))
                .foregroundStyle(Theme.textMuted)
                .padding(.horizontal, 10)
                .frame(minHeight: 36)
                .contentShape(Rectangle())
            }
            .buttonStyle(PressWashButtonStyle())
            .padding(.horizontal, 16)
            .accessibilityIdentifier("pending-comments")
        }
    }
}

struct CommentsPanel: View {
    let drafts: CommentDrafts
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if let source = drafts.editor {
                    CommentEditor(source: source, drafts: drafts).id(source.id)
                } else {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 12) {
                            Text("These quotes and comments accompany your next message. Nothing is sent yet.")
                                .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                            if drafts.comments.isEmpty {
                                Text("No pending comments. Select text directly in the chat and choose Comment.")
                                    .font(Theme.sans(13)).foregroundStyle(Theme.textFaint)
                            }
                            ForEach(drafts.comments) { comment in
                                HStack(alignment: .top, spacing: 6) {
                                    Button { drafts.edit(comment) } label: {
                                        VStack(alignment: .leading, spacing: 6) {
                                            Text(comment.quote).lineLimit(2).foregroundStyle(Theme.textFaint)
                                            Text(comment.comment).lineLimit(3).foregroundStyle(Theme.text)
                                        }
                                        .font(Theme.sans(13))
                                        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                                        .contentShape(Rectangle())
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityHint("Edit comment")
                                    Button { drafts.remove(comment.id) } label: {
                                        Image(systemName: "xmark").font(.system(size: 11, weight: .medium))
                                            .frame(width: 44, height: 44)
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityLabel("Remove comment")
                                }
                                .padding(12)
                                .background(whiteAlpha(0.04), in: RoundedRectangle(cornerRadius: 12))
                            }
                        }
                        .padding(20)
                    }
                    .navigationTitle("Comments")
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { dismiss() }
                        }
                    }
                }
            }
            .background(SheetStyle.panel)
            .navigationBarTitleDisplayMode(.inline)
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }
}

private struct CommentEditor: View {
    let source: CommentSource
    let drafts: CommentDrafts
    @Environment(\.dismiss) private var dismiss
    @State private var comment: String
    @State private var error: String?

    init(source: CommentSource, drafts: CommentDrafts) {
        self.source = source
        self.drafts = drafts
        _comment = State(initialValue: source.comment)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text("Selected quote")
                    .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                ScrollView {
                    Text(source.text)
                        .font(Theme.sans(14)).foregroundStyle(Theme.textMuted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                }
                .frame(maxHeight: 180)
                .padding(12)
                .background(whiteAlpha(0.04), in: RoundedRectangle(cornerRadius: 12))
                Text("Comment")
                    .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                TextField("What should the agent do with this quote?", text: $comment, axis: .vertical)
                    .font(Theme.sans(15)).lineLimit(3...8)
                    .padding(12)
                    .background(whiteAlpha(0.04), in: RoundedRectangle(cornerRadius: 12))
                    .accessibilityIdentifier("comment-input")
                Text("Saved to this session's next input. Saving does not send a message.")
                    .font(Theme.sans(12)).foregroundStyle(Theme.textFaint)
                if let error { Text(error).font(Theme.sans(12)).foregroundStyle(Theme.danger) }
            }
            .padding(20)
        }
        .navigationTitle(source.editingId == nil ? "Comment" : "Edit comment")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save") { error = drafts.save(source: source, quote: source.text, comment: comment) }
                    .disabled(CommentPrompt.normalize(source.text).isEmpty
                              || comment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("save-comment")
            }
        }
    }
}
