// Home list rows and components: section headers, device tabs, project and
// chat rows, activity badges, status marks, relative time.

import SwiftUI

// MARK: - Rows

extension View {
    /// Cell paint shared by every inset-grouped list in the app.
    func groupedRowStyle() -> some View {
        listRowBackground(Theme.groupedRow)
            .listRowSeparatorTint(Theme.border)
    }
}

/// Sentence-case section header in the app's type, not the system's caps.
/// Keeps the system position (aligned with row text), per the list style.
struct ListSectionHeader<Accessory: View>: View {
    let title: String
    @ViewBuilder var accessory: Accessory

    var body: some View {
        HStack(spacing: 8) {
            Text(title)
                .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
                .foregroundStyle(Theme.textMuted)
                .lineLimit(1)
            Spacer(minLength: 8)
            accessory
        }
        .textCase(nil)
    }
}

extension ListSectionHeader where Accessory == EmptyView {
    init(title: String) {
        self.init(title: title) { EmptyView() }
    }
}

/// "All" plus one rounded-rectangle tab per project-owning device, with its
/// presence dot. Content-layer filters, so solid fills rather than glass
/// (glass belongs to the navigation layer). Scrolls sideways past ~4 devices.
struct DeviceTabs: View {
    @Environment(AppModel.self) private var model
    let deviceIds: [String]
    @Binding var selection: String

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                tab(id: "", title: "All", online: nil)
                ForEach(deviceIds, id: \.self) { id in
                    tab(id: id, title: model.deviceName(id), online: model.deviceOnline(id))
                }
            }
        }
        // Tabs start on the cards' edge (the inset-grouped section margin)
        // but scroll out to the screen edge.
        .contentMargins(.horizontal, Self.margin, for: .scrollContent)
    }

    /// The inset-grouped section margin.
    static let margin: CGFloat = 16

    private func tab(id: String, title: String, online: Bool?) -> some View {
        // A filter naming a vanished device reads as All.
        let current = deviceIds.contains(selection) ? selection : ""
        let selected = id == current
        return Button {
            guard !selected else { return }
            UISelectionFeedbackGenerator().selectionChanged()
            withAnimation(Motion.fadeQuick) { selection = id }
        } label: {
            HStack(spacing: 6) {
                if let online {
                    Circle()
                        .fill(online ? Theme.statusCompleted : Theme.textFaint.opacity(0.5))
                        .frame(width: 6, height: 6)
                }
                Text(title)
                    .font(Theme.sans(14, weight: .medium, relativeTo: .subheadline))
                    .foregroundStyle(selected ? Theme.bg : Theme.text)
                    .lineLimit(1)
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 36)
            .background(
                selected ? Theme.text : Theme.groupedRow,
                in: RoundedRectangle(cornerRadius: 14, style: .continuous)
            )
            .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(online.map { "\(title), \($0 ? "online" : "offline")" } ?? title)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The name with the session count across from it; then the project's
/// activity as colored badges, a state each in the session rows' own marks,
/// or when it was last active once nothing is happening.
struct ProjectRow: View {
    @Environment(AppModel.self) private var model
    let space: Space

    var body: some View {
        let chats = model.chats(in: space.id)
        let counts = ChatIndicator.activityCounts(chats.map { model.indicator(for: $0) })
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(space.displayName)
                    .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Spacer(minLength: 8)
                if !chats.isEmpty {
                    Text(chats.count == 1 ? "1 session" : "\(chats.count) sessions")
                        .font(Theme.sans(12.5, relativeTo: .footnote))
                        .foregroundStyle(Theme.textFaint)
                        .lineLimit(1)
                        .fixedSize()
                }
            }
            if counts.isEmpty {
                Text(quietLine(chats))
                    .font(Theme.sans(13, relativeTo: .subheadline))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            } else {
                ActivityBadges(counts: counts)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    /// The second line of a project with nothing going on.
    private func quietLine(_ chats: [Chat]) -> String {
        guard let last = chats.map({ $0.lastMessageAt ?? $0.createdAt }).max() else { return "No sessions yet" }
        let ago = relativeTime(last)
        return ago == "now" ? "Last active just now" : "Last active \(ago) ago"
    }
}

/// One badge per state with sessions in it, attention first: the session
/// rows' mark and color, the count and the state in words. Where the words
/// don't fit, the badges keep only their marks and counts.
struct ActivityBadges: View {
    let counts: [ChatIndicator.Count]

    var body: some View {
        ViewThatFits(in: .horizontal) {
            badges(compact: false)
            badges(compact: true)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(ChatIndicator.activitySummary(counts).joined(separator: ", "))
    }

    private func badges(compact: Bool) -> some View {
        HStack(spacing: 6) {
            ForEach(counts, id: \.indicator) { count in
                ActivityBadge(count: count, compact: compact)
            }
        }
        .fixedSize()
    }
}

struct ActivityBadge: View {
    let count: ChatIndicator.Count
    var compact = false

    var body: some View {
        let color = SessionStatusMark.color(count.indicator)
        HStack(spacing: 5) {
            SessionStatusMark(indicator: count.indicator, cellSize: 2.2)
            Text(compact ? "\(count.count)" : count.label)
                .font(Theme.sans(12, weight: .medium, relativeTo: .footnote))
                .foregroundStyle(color)
                .lineLimit(1)
        }
        .padding(.horizontal, 8)
        .frame(height: 22)
        .background(color.opacity(0.13), in: Capsule())
    }
}

extension ChatIndicator {
    /// How many sessions are in one state.
    struct Count: Hashable {
        var indicator: ChatIndicator
        var count: Int

        /// "1 running", "2 need input", "1 failed", "3 done".
        var label: String {
            switch indicator {
            case .awaitingInput: return count == 1 ? "1 needs input" : "\(count) need input"
            case .errored: return "\(count) failed"
            case .working: return "\(count) running"
            case .completed: return "\(count) done"
            case .idle: return "\(count) idle"
            }
        }
    }

    /// The states with sessions in them, attention first: needs input,
    /// failed, running, done. Done/failed count unread runs only (the
    /// indicator's meaning); idle sessions aren't activity.
    static func activityCounts(_ indicators: [ChatIndicator]) -> [Count] {
        [ChatIndicator.awaitingInput, .errored, .working, .completed].compactMap { state in
            let n = indicators.filter { $0 == state }.count
            return n > 0 ? Count(indicator: state, count: n) : nil
        }
    }

    /// "1 needs input · 1 running · 1 done" parts, in the badges' order.
    static func activitySummary(_ indicators: [ChatIndicator]) -> [String] {
        activitySummary(activityCounts(indicators))
    }

    static func activitySummary(_ counts: [Count]) -> [String] {
        counts.map(\.label)
    }
}

/// Session row with one reading edge: live status in a leading gutter
/// (Mail's unread-dot slot), the title, then one muted line — checkout (or,
/// outside a project, "project @ device") and time. The trailing edge is
/// left to the disclosure chevron.
struct ChatRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dynamicTypeSize) private var typeSize
    /// Lifts the mark's center from the title baseline to mid x-height.
    @ScaledMetric(relativeTo: .body) private var markLift: CGFloat = 5.5
    let chat: Chat
    var showLocation: Bool
    /// Reserve the status slot. The list decides, so its titles share one
    /// edge: set when any of its rows has a status (see `needsStatusSlot`).
    var statusSlot: Bool

    /// A list of all-idle sessions drops the slot rather than indent every
    /// title past an empty gutter.
    static func needsStatusSlot(_ chats: [Chat], in model: AppModel) -> Bool {
        chats.contains { model.indicator(for: $0) != .idle }
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            if statusSlot {
                // Idle keeps the slot, so every title starts on the same edge.
                SessionStatusMark(indicator: model.indicator(for: chat))
                    .frame(width: 10)
                    .alignmentGuide(.firstTextBaseline) { [markLift] in $0[VerticalAlignment.center] + markLift }
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(chat.displayTitle)
                    .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    // Accessibility sizes leave room for ~2 words per line.
                    .lineLimit(typeSize.isAccessibilitySize ? 2 : 1)
                HStack(spacing: 6) {
                    // Pi is the only new-session harness; mark the exceptions.
                    if let harness = chat.config?.harness, harness != "pi" {
                        HarnessBadge(harness: harness, size: 12, neutral: Theme.textMuted)
                    }
                    // The time never truncates; the checkout gives way first.
                    HStack(spacing: 0) {
                        Text(showLocation ? location : checkoutLabel)
                            .lineLimit(1)
                            .truncationMode(showLocation ? .tail : .middle)
                        Text(" · \(relativeTime(chat.lastMessageAt ?? chat.createdAt))")
                            .fixedSize()
                    }
                }
                .font(Theme.sans(13, relativeTo: .subheadline))
                .foregroundStyle(Theme.textMuted)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var isWorktree: Bool {
        guard let cwd = chat.cwd, !cwd.isEmpty, let space = model.space(for: chat) else { return false }
        return (cwd as NSString).standardizingPath != (space.path as NSString).standardizingPath
    }

    private var checkoutLabel: String {
        let branch = chat.branch?.trimmingCharacters(in: .whitespacesAndNewlines)
        let branchLabel = branch.flatMap { $0.isEmpty ? nil : $0 }
        if isWorktree, let cwd = chat.cwd {
            if let branchLabel { return "\(branchLabel) (worktree)" }
            let name = ((cwd as NSString).standardizingPath as NSString).lastPathComponent
            return "Worktree \(name)"
        }
        return branchLabel ?? "Current checkout"
    }

    /// "space @ device" (the session header's format). The space name (not
    /// the cwd basename) is what the desktop row shows — they differ once a
    /// space has been renamed, or when the session runs in a worktree off to
    /// the side.
    private var location: String {
        // The folder is named after the chat id — the device says it all.
        if chat.isScratch { return model.deviceName(chat.deviceId) }
        let space =
            model.space(for: chat)?.displayName
            ?? chat.cwd.map { ($0 as NSString).lastPathComponent }
            ?? "?"
        return "\(space) @ \(model.deviceName(chat.deviceId))"
    }
}

/// Live status as a bare glyph for the row's leading slot (the label is
/// spoken, not shown); blank when idle.
struct SessionStatusMark: View {
    let indicator: ChatIndicator
    var cellSize: CGFloat = 2.6

    var body: some View {
        Group {
            switch indicator {
            case .working:
                MiniSpinner(cellSize: cellSize)
            case .completed:
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .bold))
            case .awaitingInput, .errored:
                Circle().frame(width: 8, height: 8)
            case .idle:
                Color.clear.frame(width: 8, height: 8)
            }
        }
        .foregroundStyle(Self.color(indicator))
        .fixedSize()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label ?? "")
        .accessibilityHidden(label == nil)
    }

    private var label: String? {
        switch indicator {
        case .working: return "Working"
        case .awaitingInput: return "Needs input"
        case .errored: return "Failed"
        case .completed: return "Done"
        case .idle: return nil
        }
    }

    static func color(_ indicator: ChatIndicator) -> Color {
        switch indicator {
        case .working: return Theme.statusWorking
        case .awaitingInput: return Theme.accent
        case .errored: return Theme.danger
        case .completed, .idle: return Theme.statusCompleted
        }
    }
}

func relativeTime(_ ms: Int64) -> String {
    let delta = max(0, nowMs() - ms) / 1000
    if delta < 60 { return "now" }
    if delta < 3600 { return "\(delta / 60)m" }
    if delta < 86_400 { return "\(delta / 3600)h" }
    return "\(delta / 86_400)d"
}

/// Weak handle onto the NavigationStack's UIKit controller (see
/// `HomeView.navigation`).
@MainActor
final class NavigationProbe {
    weak var controller: UINavigationController?
    var transitioning: Bool { controller?.transitionCoordinator != nil }
}

/// Zero-size view inside the stack's root whose responder chain climbs
/// through the hosting controller to the stack's UINavigationController.
struct NavigationProbeView: UIViewRepresentable {
    let probe: NavigationProbe

    func makeUIView(context: Context) -> ProbeView {
        let view = ProbeView()
        view.probe = probe
        view.isUserInteractionEnabled = false
        view.backgroundColor = .clear
        return view
    }

    func updateUIView(_ view: ProbeView, context: Context) {
        view.probe = probe
        view.capture()
    }

    final class ProbeView: UIView {
        var probe: NavigationProbe?
        override func didMoveToWindow() {
            super.didMoveToWindow()
            capture()
        }
        override func layoutSubviews() {
            super.layoutSubviews()
            capture()
        }
        func capture() {
            guard let probe, probe.controller == nil else { return }
            var responder: UIResponder? = next
            while let current = responder {
                if let nav = current as? UINavigationController {
                    probe.controller = nav
                    return
                }
                if let controller = current as? UIViewController, let nav = controller.navigationController {
                    probe.controller = nav
                    return
                }
                responder = current.next
            }
        }
    }
}
