// The workspace chat's other transcript cards: a held memory write asking
// the owner where it was made (transcript/memory_card.rs; docs/design/
// memory.md §7), and the line over a routine's post naming the routine,
// when it ran and how, with its session one tap away.

import SwiftUI

// MARK: - Held memory write

struct MemoryWriteCard: View {
    @Environment(AppModel.self) private var model
    let callId: String

    @State private var busy = false
    @State private var error: String?

    /// Lines of the proposed text a card shows before it fades out.
    static let previewLines = 6

    var body: some View {
        switch model.botRecords.memoryCard(callId) {
        case .held(let store, let write):
            held(store: store, write: write)
        case .decided(let saved):
            decided(saved: saved)
        case nil:
            EmptyView()
        }
    }

    private func held(store: MemoryStore, write: HeldWrite) -> some View {
        BotCardSurface {
            HStack(spacing: 10) {
                BotCardTile(symbol: "book.closed", tint: Theme.warning)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Save to memory?")
                        .font(Theme.sans(15, weight: .semibold, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                    Text("\(Self.fileName(write.path)) · in \(storeWords(store.scope))")
                        .font(Theme.sans(12, relativeTo: .footnote))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .accessibilityElement(children: .combine)
            BotCardRule()
            VStack(alignment: .leading, spacing: 8) {
                proposed(write.preview)
                Text(Self.reasonLine(write.reason))
                    .font(Theme.sans(12, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            BotCardRule()
            VStack(alignment: .leading, spacing: 10) {
                Text(error ?? "Not read by any run until you save it.")
                    .font(Theme.sans(12.5, relativeTo: .footnote))
                    .foregroundStyle(error == nil ? Theme.textMuted : Theme.danger)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Spacer(minLength: 0)
                    BotCardButton(title: "Don't save", role: .secondary) {
                        decide(store.scope, .discard(id: write.id))
                    }
                    BotCardButton(title: "Save", role: .primary, busy: busy) {
                        decide(store.scope, .accept(id: write.id))
                    }
                }
                .disabled(busy)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("memory-card")
    }

    /// What the write would leave in the file: its first lines, fading out
    /// when there is more; a quiet line for an empty file or a delete.
    @ViewBuilder
    private func proposed(_ preview: String?) -> some View {
        if let preview, !preview.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            let (shown, more) = Self.previewLines(preview)
            Text(shown)
                .font(Theme.mono(12))
                .foregroundStyle(Theme.text.opacity(0.9))
                .lineLimit(Self.previewLines)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                .background(Theme.text.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
                .mask {
                    LinearGradient(
                        stops: [
                            .init(color: .black, location: more ? 0.7 : 1),
                            .init(color: .black.opacity(more ? 0.25 : 1), location: 1),
                        ],
                        startPoint: .top, endPoint: .bottom)
                }
        } else {
            Text(preview == nil ? "Deletes the file." : "Leaves the file empty.")
                .font(Theme.sans(12.5, relativeTo: .footnote))
                .foregroundStyle(Theme.textMuted)
        }
    }

    /// The answer, as one line like a resolved question's chip.
    private func decided(saved: Bool) -> some View {
        HStack(spacing: 8) {
            Image(systemName: saved ? "checkmark" : "book.closed")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(saved ? Theme.success : Theme.textMuted)
                .frame(width: 20)
            Text(saved ? "Saved to memory" : "Not saved")
                .font(Theme.sans(12.5, weight: .medium, relativeTo: .footnote))
                .foregroundStyle(Theme.textMuted)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .frame(minHeight: 34)
        .background(Theme.text.opacity(0.045), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Theme.border, lineWidth: 1))
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("memory-card-decided")
    }

    /// Answer the write. On success the store's row moves it to its
    /// decisions and the card follows.
    private func decide(_ scope: MemoryScope, _ edit: MemoryEdit) {
        guard !busy else { return }
        busy = true
        error = nil
        Task { @MainActor in
            defer { busy = false }
            do {
                _ = try await model.workspaceData().editMemory(scope: scope, edit: edit)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    /// Which memory a store is, mid-sentence: "your memory", "cypher's
    /// memory" (memory_card.rs `store_words`).
    private func storeWords(_ scope: MemoryScope) -> String {
        switch scope {
        case .you: return "your memory"
        case .project(let spaceId):
            guard let name = model.spaces.first(where: { $0.id == spaceId })?.displayName else {
                return "the project's memory"
            }
            return "\(name)'s memory"
        case .workspace: return "the workspace's memory"
        case .role(let agent, _): return "\(agent)'s memory"
        }
    }

    /// A path's last segment: `/project/decisions/rounding.md` →
    /// `rounding.md`.
    static func fileName(_ path: String) -> String {
        let trimmed = path.trimmingCharacters(in: .whitespaces)
        let path = trimmed.hasSuffix("/") ? String(trimmed.dropLast()) : trimmed
        return path.split(separator: "/").last.map(String.init) ?? path
    }

    /// The first lines of the proposed text, and whether there is more.
    static func previewLines(_ text: String) -> (String, Bool) {
        let text = text.replacingOccurrences(of: "\\s+$", with: "", options: .regularExpression)
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let shown = lines.prefix(previewLines).joined(separator: "\n")
        return (shown, lines.count > previewLines || text.hasSuffix("…") || shown.count > 300)
    }

    /// Why it was held, as a sentence (memory_card.rs `reason_line`).
    static func reasonLine(_ reason: String) -> String {
        var reason = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        while reason.hasSuffix(".") { reason.removeLast() }
        return reason.isEmpty
            ? "Held because this session read untrusted content" : "Held because this session \(reason)"
    }
}

// MARK: - Routine post

/// The line over a routine's post: the routine, when it ran, how it went,
/// and its session.
struct PostAttribution: View {
    @Environment(\.openBotChat) private var openChat
    let from: BotPost

    var body: some View {
        if case .routine(_, let name, let outcome, let runAt, let threadChatId) = from {
            HStack(spacing: 10) {
                BotCardTile(symbol: "clock", tint: Theme.accent)
                VStack(alignment: .leading, spacing: 1) {
                    Text(name)
                        .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    Text("Routine · \(BotTime.past(runAt))")
                        .font(Theme.sans(12, relativeTo: .footnote))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
                .accessibilityElement(children: .combine)
                Spacer(minLength: 4)
                BotStatusText(outcome.status, size: 12.5)
                    .fixedSize()
                if let threadChatId, let openChat {
                    Button("Open session") { openChat(threadChatId) }
                        .font(Theme.sans(12.5, weight: .medium, relativeTo: .footnote))
                        .foregroundStyle(Theme.textMuted)
                        .buttonStyle(.plain)
                        .padding(.leading, 4)
                        .frame(minHeight: 32)
                        .contentShape(Rectangle())
                        .accessibilityIdentifier("post-open-session")
                }
            }
            .padding(.bottom, 2)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("routine-post")
        }
    }
}
