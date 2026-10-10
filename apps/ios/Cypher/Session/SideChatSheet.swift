// Side chat sheet — the desktop's right-pane Side Chat tab as a sheet over
// the session: the quoted selection on top, the side chat's own transcript
// and composer below. Closing discards it on the host; "Open as Chat" keeps
// it as a normal session and opens it.

import SwiftUI

extension SideChatStore: Identifiable {}

struct SideChatSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let store: SideChatStore
    /// Called with the promoted chat's id; the presenter navigates.
    let onPromoted: (String) -> Void

    @State private var scroll = ScrollState()
    @State private var catalog = RemotePiCatalog()
    @State private var promoting = false
    @State private var promoteError: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                quoteBar
                content
            }
            .background(Theme.bg.ignoresSafeArea())
            .navigationTitle("Side Chat")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Close", systemImage: "xmark") { dismiss() }
                        .accessibilityIdentifier("side-chat-close")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        promote()
                    } label: {
                        if promoting { ProgressView() } else { Text("Open as Chat") }
                    }
                    .disabled(store.phase != .open || store.session.entries.isEmpty || promoting)
                    .accessibilityIdentifier("side-chat-promote")
                }
            }
        }
        // This transcript isn't the session's: no comments, forks or nested
        // side chats from inside it.
        .environment(\.commentDrafts, nil)
        .environment(\.transcriptSelectionActions, nil)
        .task { await store.start() }
    }

    private var quoteBar: some View {
        HStack(alignment: .top, spacing: 10) {
            RoundedRectangle(cornerRadius: 1.5)
                .fill(Theme.accent)
                .frame(width: 3)
            Text(store.quote)
                .font(Theme.sans(13, relativeTo: .footnote))
                .foregroundStyle(Theme.textMuted)
                .lineLimit(3)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Quoted: \(store.quote)")
    }

    @ViewBuilder private var content: some View {
        switch store.phase {
        case .starting:
            VStack(spacing: 12) {
                CypherPulse()
                Text("Starting side chat…")
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textFaint)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .ended(let message):
            ContentUnavailableView {
                Label("Side chat ended", systemImage: "bubble.left.and.exclamationmark.bubble.right")
            } description: {
                Text(message)
            }
        case .open:
            let chat = store.chat
            TranscriptView(store: store.session, chatId: chat.id, scroll: scroll)
                .id(chat.id)
                .overlay {
                    if store.session.entries.isEmpty, store.session.pendingSends.isEmpty {
                        Text("Ask anything about the selection. The agent sees it and the recent conversation.")
                            .font(Theme.sans(14))
                            .foregroundStyle(Theme.textFaint)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 40)
                            .allowsHitTesting(false)
                    }
                }
                .safeAreaBar(edge: .bottom, spacing: 0) {
                    VStack(spacing: 6) {
                        if let error = store.error ?? promoteError {
                            Text(error)
                                .font(Theme.sans(12))
                                .foregroundStyle(Theme.danger)
                                .padding(.horizontal, 24)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        if store.status == .working {
                            HStack(spacing: 6) {
                                WorkingSpinner()
                                Text("Working…")
                                    .font(Theme.sans(12))
                                    .foregroundStyle(Theme.textMuted)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.leading, 26)
                        }
                        if let request = store.session.openInputRequest {
                            QuestionPanel(requestId: request.requestId, questions: request.questions,
                                          maximumHeight: 420, canRespond: true, stop: {
                                _ = store.session.sendInterrupt()
                            }) { requestId, answers in
                                _ = store.session.respondInput(requestId: requestId, answers: answers)
                            }
                            .id(request.requestId)
                        } else {
                            ComposerView(store: store.session, chat: chat, runLive: store.status == .working,
                                         catalog: catalog, sideChat: true)
                        }
                    }
                    .padding(.bottom, 8)
                    .onGeometryChange(for: CGFloat.self) { $0.frame(in: .global).minY } action: { [scroll] new in
                        scroll.insetTopGlobalY = new
                        scroll.insetTopChangedAt = Date().timeIntervalSinceReferenceDate
                    }
                }
        }
    }

    private func promote() {
        promoting = true
        promoteError = nil
        Task { @MainActor in
            defer { promoting = false }
            do {
                let chatId = try await store.promote(demo: model.demo)
                dismiss()
                onPromoted(chatId)
            } catch {
                promoteError = "Couldn't open it as a chat — \(error.localizedDescription)"
            }
        }
    }
}
