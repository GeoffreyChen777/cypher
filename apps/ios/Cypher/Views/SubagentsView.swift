import SwiftUI

private extension SubagentPanelStatus {
    var color: Color {
        switch self {
        case .running: return Theme.accent
        case .starting: return Theme.textFaint
        case .stale: return Theme.warning
        case .done: return Theme.statusCompleted
        case .error: return Theme.danger
        }
    }
    var symbol: String {
        switch self {
        case .running: return "circle.fill"
        case .starting: return "circle.dotted"
        case .stale: return "exclamationmark.triangle"
        case .done: return "checkmark"
        case .error: return "xmark"
        }
    }
}

/// Current-session chrome, above the composer's trailing edge.
struct SubagentsAccessory: View {
    @Environment(AppModel.self) private var model
    let parent: Chat
    let store: SessionStore
    let maxWidth: CGFloat
    let openChild: (String) -> Void
    @State private var showDetails = false
    @State private var pendingChild: SubagentPanelEntry?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { _ in
            let entries = model.subagents(for: parent, store: store, now: nowMs())
            let counts = SubagentProjection.counts(entries)
            if counts.total > 0 {
                Button { showDetails = true } label: {
                    HStack(spacing: 5) {
                        if counts.running > 0 {
                            MiniSpinner()
                        } else {
                            let status: SubagentPanelStatus = counts.starting > 0 ? .starting
                                : counts.stale > 0 ? .stale : counts.failed > 0 ? .error : .done
                            Image(systemName: status.symbol).foregroundStyle(status.color)
                        }
                        Text(counts.compact)
                            .lineLimit(1).truncationMode(.tail)
                        Image(systemName: "chevron.up")
                            .font(.system(size: 8, weight: .semibold))
                    }
                    .font(Theme.sans(11, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, 8)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(PressWashButtonStyle())
                .frame(maxWidth: maxWidth, alignment: .trailing)
                .accessibilityIdentifier("subagents-trigger")
                .accessibilityLabel("Subagents: \(counts.summary)")
                .accessibilityHint("Show progress and open child sessions")
            }
        }
        .sheet(isPresented: $showDetails, onDismiss: {
            defer { pendingChild = nil }
            // Recheck after sheet dismissal: a stale/foreign snapshot id is
            // never sufficient to navigate to an arbitrary workspace chat.
            if let entry = pendingChild, let currentParent = model.chat(id: parent.id),
               let child = SubagentProjection.navigableChild(entry, parent: currentParent, chats: model.allChats) {
                openChild(child.id)
            }
        }) {
            SubagentsSheet(parentId: parent.id, store: store) { entry in
                pendingChild = entry
                showDetails = false
            }
        }
        .onAppear {
            if model.launchSheet == "subagents" {
                model.launchSheet = nil
                showDetails = true
            }
        }
        .onDisappear { showDetails = false }
    }
}

struct SubagentsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let parentId: String
    let store: SessionStore
    let openChild: (SubagentPanelEntry) -> Void

    var body: some View {
        NavigationStack {
            TimelineView(.periodic(from: .now, by: 1)) { _ in
                let parent = model.chat(id: parentId)
                let entries = parent.map { model.subagents(for: $0, store: store, now: nowMs()) } ?? []
                let counts = SubagentProjection.counts(entries)
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        if counts.total == 0 {
                            Text("No subagent activity in this session.")
                                .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
                        } else {
                            Text(counts.summary)
                                .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                            Text("Finished \(counts.done + counts.failed) of \(counts.total) · \(counts.done) succeeded")
                                .font(Theme.sans(11)).foregroundStyle(Theme.textFaint)
                            if let parent, !model.deviceOnline(parent.deviceId), model.demo == nil {
                                Text("Device offline · showing last synced activity")
                                    .font(Theme.sans(12)).foregroundStyle(Theme.warning)
                            }
                            ForEach(entries) { entry in
                                let child = parent.flatMap {
                                    SubagentProjection.navigableChild(entry, parent: $0, chats: model.allChats)
                                }
                                SubagentDetailRow(entry: entry, canOpen: child != nil,
                                    awaitingInput: child.flatMap { model.sessionRows[$0.id]?.status } == .awaitingInput) {
                                    openChild(entry)
                                }
                            }
                        }
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 12)
                }
            }
            .background(SheetStyle.panel)
            .navigationTitle("Subagents")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }
}

private struct SubagentDetailRow: View {
    let entry: SubagentPanelEntry
    let canOpen: Bool
    let awaitingInput: Bool
    let open: () -> Void

    var body: some View {
        if canOpen {
            Button(action: open) { card }
                .buttonStyle(PressWashButtonStyle(cornerRadius: 14))
                .accessibilityIdentifier("subagent-open-\(entry.id)")
                .accessibilityHint("Open subagent session")
        } else {
            card.textSelection(.enabled)
        }
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Group {
                    if entry.status == .running {
                        MiniSpinner(cellSize: 1.6)
                    } else {
                        Image(systemName: entry.status.symbol)
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(entry.status.color)
                    }
                }
                .frame(width: 14, height: 14)
                Text(entry.agent).font(Theme.sans(14, weight: .medium))
                Spacer(minLength: 8)
                Text(awaitingInput && entry.status == .running ? "Awaiting input" : entry.status.label)
                    .font(Theme.sans(11)).foregroundStyle(entry.status.color)
                if canOpen {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                }
            }
            Text(entry.task.isEmpty ? "No task description" : entry.task)
                .font(Theme.sans(13)).foregroundStyle(Theme.textMuted)
            HStack {
                Text(entry.mode.rawValue)
                if let model = entry.model { Text(model).lineLimit(1).truncationMode(.middle) }
            }
            .font(Theme.mono(10)).foregroundStyle(Theme.textFaint)
            if let progress = entry.progress, !progress.isEmpty {
                Text(progress).font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
                    .lineLimit(8)
            }
            if entry.status == .stale {
                Text("No recent heartbeat. This does not mean the task finished.")
                    .font(Theme.sans(11)).foregroundStyle(Theme.warning)
            }
            if !canOpen {
                Text(entry.childChatId == nil
                     ? "This run has no linked Cypher session."
                     : "Child session not synced or no longer available.")
                    .font(Theme.sans(11)).foregroundStyle(Theme.textFaint)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        .contentShape(RoundedRectangle(cornerRadius: 14))
        .background(whiteAlpha(0.04), in: RoundedRectangle(cornerRadius: 14))
        .foregroundStyle(Theme.text)
    }
}
