// Settings › Memory (crates/ui/src/settings/memory.rs, docs/design/
// memory.md §8): what agents keep between sessions, and the one place to
// read and change it. Yours first (every session and workspace reads it),
// then each project's, then each workspace's; a row opens the store's page.
// A sheet from Home's account menu, with its own stack.

import SwiftUI

struct MemorySettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var path: [BotPage] = []

    var body: some View {
        let listing = MemoryListing(stores: model.botRecords.memoryStores, spaces: model.spaces, bots: model.bots)
        NavigationStack(path: $path) {
            List {
                Section {
                    NavigationLink(value: BotPage.memory(.you)) {
                        MemoryStoreRow(
                            name: "Yours", store: listing.you,
                            leading: AnyView(MemoryStoreGlyph(symbol: "person.crop.circle")))
                    }
                    .groupedRowStyle()
                    .accessibilityIdentifier("memory-store-you")
                } header: {
                    Text(
                        "What agents learn from working with you and keep between sessions, as files you can read and edit."
                    )
                    .font(Theme.sans(14, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .textCase(nil)
                    .padding(.bottom, 8)
                } footer: {
                    Text("Every session and workspace reads it.")
                }
                Section {
                    if listing.projects.isEmpty {
                        Text("None yet. Sessions keep what they learn about their project here.")
                            .font(Theme.sans(14, relativeTo: .body))
                            .foregroundStyle(Theme.textMuted)
                            .groupedRowStyle()
                    }
                    ForEach(listing.projects) { listed in
                        NavigationLink(value: BotPage.memory(listed.store.scope)) {
                            MemoryStoreRow(
                                name: listed.name, store: listed.store,
                                leading: AnyView(MemoryStoreGlyph(symbol: "folder")))
                        }
                        .groupedRowStyle()
                    }
                } header: {
                    ListSectionHeader(title: "Projects")
                } footer: {
                    Text("Each read by the sessions in its project.")
                }
                if !listing.workspaces.isEmpty {
                    Section {
                        ForEach(listing.workspaces) { listed in
                            NavigationLink(value: BotPage.memory(listed.store.scope)) {
                                MemoryStoreRow(name: listed.name, store: listed.store, leading: workspaceAvatar(listed))
                            }
                            .groupedRowStyle()
                        }
                    } header: {
                        ListSectionHeader(title: "Workspaces")
                    } footer: {
                        Text("Each kept and read by its workspace.")
                    }
                }
            }
            .memoryPageStyle(title: "Memory")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .navigationDestination(for: BotPage.self) { page in
                // Only memory pages are pushed here; a store page names no
                // workspace of its own.
                BotPageView(botId: "", page: page)
            }
        }
        .task {
            // Launch rig: `-sheet memory:you|project:<id>|workspace:<id>`
            // opens on that store.
            guard let spec = model.launchSheet, spec.hasPrefix("memory:") else { return }
            model.launchSheet = nil
            let rest = spec.dropFirst("memory:".count)
            let parts = rest.split(separator: ":", maxSplits: 1).map(String.init)
            switch (parts.first, parts.count > 1 ? parts[1] : nil) {
            case ("you", _): path = [.memory(.you)]
            case ("project", let id?): path = [.memory(.project(spaceId: id))]
            case ("workspace", let id?): path = [.memory(.workspace(botId: id))]
            default: break
            }
        }
    }

    private func workspaceAvatar(_ listed: ListedMemoryStore) -> AnyView {
        guard case .workspace(let botId) = listed.store.scope, let bot = model.bot(id: botId) else {
            return AnyView(MemoryStoreGlyph(symbol: "square.grid.2x2"))
        }
        return AnyView(BotAvatar(bot: bot, size: 28))
    }
}

/// A store's row: its name, then "4 files · 1 to review · updated 3h ago"
/// (the review count in amber).
private struct MemoryStoreRow: View {
    let name: String
    let store: MemoryStore?
    let leading: AnyView

    var body: some View {
        HStack(spacing: 12) {
            leading
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .font(Theme.sans(16, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text(facts)
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    private var facts: AttributedString {
        guard let store else { return AttributedString("Nothing yet") }
        var line = AttributedString(MemoryFormat.count(store.files, "file", "files"))
        if store.pending > 0 {
            var review = AttributedString("\(store.pending) to review")
            review.foregroundColor = Theme.warning
            line += AttributedString(" · ") + review
        }
        if store.updatedAt > 0 {
            line += AttributedString(" · updated \(BotTime.ago(store.updatedAt))")
        }
        return line
    }
}

/// A store's glyph on a neutral tile, the size of a workspace avatar.
private struct MemoryStoreGlyph: View {
    let symbol: String

    var body: some View {
        RoundedRectangle(cornerRadius: 8, style: .continuous)
            .fill(Theme.text.opacity(0.08))
            .frame(width: 28, height: 28)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.textMuted)
            }
            .accessibilityHidden(true)
    }
}
