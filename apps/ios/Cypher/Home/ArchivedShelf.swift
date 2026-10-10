// Archived shelf — the desktop sidebar's settled shelf for archived sessions
// (shell/spaces.rs `render_archived_section`), sitting under the active list
// as its own inset-grouped section: a header that folds (open by default,
// session-transient), single-line rows, and Show-more paging (10, then +25).
// The desktop's hover-swapped Unarchive pill becomes swipe-to-unarchive here,
// mirroring the active rows' swipe-to-archive.

import SwiftUI

private extension Chat {
    var shelfRowId: String { "archived-\(id)" }
}

struct ArchivedSection: View {
    @Environment(AppModel.self) private var model
    /// Scope, matching the list above it: nil = All.
    var spaceId: String?
    var orphanedOnly = false

    // spaces.rs INITIAL/PAGE. Both session-transient, like the desktop's.
    @State private var open = true
    @State private var shown = ArchivedSection.initialCount
    private static let initialCount = 10
    private static let pageSize = 25

    var body: some View {
        let archived = model.archivedChats(in: spaceId).filter { chat in
            !orphanedOnly || !model.spaces.contains(where: { $0.id == chat.spaceId })
        }
        if !archived.isEmpty {
            Section {
                if open {
                    // Distinct identity namespace (desktop's "archived-{id}"
                    // vs "c:{id}" FLIP keys): the SAME id in both ForEach made
                    // SwiftUI animate archiving as a cross-section MOVE — the
                    // full-size row flew down through its neighbors and landed
                    // in the shelf before snapping to the slim style. With
                    // separate ids it's a clean exit + entrance.
                    ForEach(archived.prefix(shown), id: \.shelfRowId) { chat in
                        row(chat)
                    }
                    if archived.count > shown {
                        showMore(remaining: archived.count - shown)
                    }
                }
            } header: {
                header(count: archived.count)
            }
        }
    }

    private func header(count: Int) -> some View {
        Button {
            withAnimation(Motion.collapse) {
                open.toggle()
                shown = Self.initialCount
            }
        } label: {
            ListSectionHeader(title: "Archived") {
                HStack(spacing: 6) {
                    Text("\(count)")
                        .font(Theme.sans(13, relativeTo: .subheadline))
                        .foregroundStyle(Theme.textFaint)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(open ? "Collapse archived" : "Expand archived, \(count) sessions")
    }

    /// Single line: dimmed harness mark, muted title, time-ago.
    private func row(_ chat: Chat) -> some View {
        NavigationLink(value: Route.chat(chat.id)) {
            HStack(spacing: 10) {
                if let harness = chat.config?.harness, harness != "pi" {
                    HarnessBadge(harness: harness, size: 14, dimmed: true)
                }
                Text(chat.displayTitle)
                    .font(Theme.sans(15, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text(relativeTime(chat.lastMessageAt ?? chat.createdAt))
                    .font(Theme.sans(13, relativeTo: .subheadline))
                    .foregroundStyle(Theme.textFaint)
                    .fixedSize()
            }
            .padding(.vertical, 2)
        }
        .groupedRowStyle()
        .archivedSessionRowActions(chat)
    }

    private func showMore(remaining: Int) -> some View {
        Button {
            shown = max(shown, Self.initialCount) + Self.pageSize
        } label: {
            Text("Show \(min(remaining, Self.pageSize)) More")
                .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                .foregroundStyle(Theme.text)
        }
        .groupedRowStyle()
    }
}
