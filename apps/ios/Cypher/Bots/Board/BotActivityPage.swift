// A workspace's audit log (activity.rs): everything it did, newest first,
// grouped by day and filterable as Plans, Sessions, Routines, Memory or
// Connections. An entry reads as its verb (muted, red for a failure), its
// subject at full strength and a quieter detail line, and opens the work,
// session or project it names.

import SwiftUI

struct BotActivityPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let botId: String
    @State private var filter: ActivityGroup?

    var body: some View {
        let entries = model.data?.activity(of: botId, group: filter) ?? []
        BotPageList(title: "Activity") {
            ForEach(ActivityDay.group(entries)) { day in
                Section {
                    ForEach(day.entries) { entry in
                        row(entry)
                    }
                } header: {
                    ListSectionHeader(title: day.title)
                }
                .groupedRowStyle()
            }
        }
        .overlay {
            if entries.isEmpty {
                BotEmptyState(
                    symbol: "waveform.path.ecg",
                    title: filter == nil ? "No activity yet" : "Nothing of this kind yet",
                    hint: "What the workspace plans, starts, notes and runs is logged here.")
            }
        }
        // The chips stay put above the log and run to the screen's edges,
        // so the last one cut off there reads as "scroll for more".
        .safeAreaInset(edge: .top, spacing: 0) {
            chips
        }
    }

    /// One chip per kind, All first (activity.rs `FILTERS`).
    private var chips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                chip(nil)
                ForEach(ActivityGroup.allCases, id: \.self) { chip($0) }
            }
        }
        .contentMargins(.horizontal, ScopeChips.margin, for: .scrollContent)
        .mask {
            HStack(spacing: 0) {
                Color.black
                LinearGradient(colors: [.black, .black.opacity(0)], startPoint: .leading, endPoint: .trailing)
                    .frame(width: ScopeChips.margin)
            }
        }
        .padding(.vertical, 8)
        .background(Theme.surface)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Filter")
    }

    private func chip(_ group: ActivityGroup?) -> some View {
        let selected = filter == group
        let title = group?.label ?? "All"
        return Button {
            guard !selected else { return }
            UISelectionFeedbackGenerator().selectionChanged()
            withAnimation(Motion.fadeQuick) { filter = group }
        } label: {
            Text(title)
                .font(Theme.sans(14, weight: .medium, relativeTo: .subheadline))
                .foregroundStyle(selected ? Theme.bg : Theme.text)
                .lineLimit(1)
                .padding(.horizontal, 14)
                .frame(minHeight: 36)
                .background(
                    selected ? Theme.text : Theme.groupedRow,
                    in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("activity-filter-\(title)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    @ViewBuilder
    private func row(_ entry: BotActivity) -> some View {
        let link = model.data.map { data in
            ActivityLink.of(
                entry, chat: { data.activityChatLink($0) },
                hasProject: { id in data.spaces.contains { $0.id == id } })
        } ?? .none
        let label = ActivityRow(entry: entry)
        switch link {
        case .work(let id): NavigationLink(value: BotPage.workItem(id)) { label }
        case .thread(let id): NavigationLink(value: BotThreadLink(chatId: id)) { label }
        case .project(let id): NavigationLink(value: BotPage.project(id)) { label }
        case .chat(let id):
            Button {
                openChat?(id)
            } label: {
                label
            }
            .tint(Theme.text)
        case .none: label
        }
    }
}

/// An entry: its kind's glyph, the verb and subject on one line (wrapping
/// to two), the detail quieter on one, and the time of day.
struct ActivityRow: View {
    @ScaledMetric(relativeTo: .body) private var markLift: CGFloat = 5.5
    let entry: BotActivity

    var body: some View {
        let parts = entry.parts
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            BoardGlyph(symbol: Self.symbol(entry.kind.group))
                .frame(width: 18)
                .alignmentGuide(.firstTextBaseline) { [markLift] in $0[VerticalAlignment.center] + markLift }
            VStack(alignment: .leading, spacing: 2) {
                Text(line(parts))
                    .font(Theme.sans(15, relativeTo: .body))
                    .lineLimit(2)
                if let detail = parts.detail {
                    Text(detail)
                        .font(Theme.sans(13, relativeTo: .subheadline))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(Self.time(entry.at))
                .font(Theme.sans(13, relativeTo: .subheadline))
                .foregroundStyle(Theme.textFaint)
                .monospacedDigit()
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    /// The muted verb (red for a failure) and the subject at full strength;
    /// without a subject the verb is the whole entry.
    private func line(_ parts: ActivityParts) -> AttributedString {
        let failed = entry.failed
        guard let subject = parts.subject else {
            var all = AttributedString(parts.verb)
            all.foregroundColor = failed ? Theme.danger : Theme.text
            return all
        }
        var verb = AttributedString(parts.verb.isEmpty ? "" : "\(parts.verb) ")
        verb.foregroundColor = failed ? Theme.danger : Theme.textMuted
        var object = AttributedString(subject)
        object.foregroundColor = Theme.text
        object.font = Theme.sans(15, weight: .medium, relativeTo: .body)
        return verb + object
    }

    static func symbol(_ group: ActivityGroup) -> String {
        switch group {
        case .plans: "checklist"
        case .sessions: "bubble.left"
        case .routines: "clock"
        case .memory: "book"
        case .connections: "person.2"
        }
    }

    private static let timeFormat: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "HH:mm"
        return f
    }()

    static func time(_ ms: Int64) -> String {
        timeFormat.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }
}
