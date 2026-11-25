// A plan's permission level as one compact menu (docs/design/agent-bots.md
// §6.6): the three levels `/permissions` offers, under the same names. It
// reads in neutral ink; only Full access turns amber, as the composer's
// shield does. The plan card, the plan's page and the workspace's settings
// (the default for new plans) all use it.

import SwiftUI

struct PlanLevelPicker: View {
    let level: PermissionLevel
    var identifier = "plan-level"
    let onPick: (PermissionLevel) -> Void

    private var full: Bool { level.sandbox == PermissionLevel.fullAccess }

    var body: some View {
        Menu {
            Section("Permissions") {
                ForEach(PermissionLevel.all, id: \.value) { option in
                    Button {
                        guard option != level else { return }
                        onPick(option)
                    } label: {
                        if option == level {
                            Label(option.label, systemImage: "checkmark")
                        } else {
                            Text(option.label)
                        }
                        Text(option.description)
                    }
                    .accessibilityIdentifier("level-\(option.value)")
                }
            }
        } label: {
            HStack(spacing: 5) {
                LineIconView(.shieldCheck, size: 13, color: tint)
                Text(level.label)
                    .font(Theme.sans(13, weight: .medium, relativeTo: .footnote))
                    .foregroundStyle(tint)
                    .lineLimit(1)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(full ? Theme.warning : Theme.textFaint)
            }
            .padding(.horizontal, 10)
            .frame(minHeight: 28)
            .background(full ? Theme.warning.opacity(0.12) : Theme.text.opacity(0.06), in: Capsule())
            .overlay {
                if full { Capsule().strokeBorder(Theme.warning.opacity(0.25), lineWidth: 1) }
            }
            .contentShape(Capsule())
            .fixedSize()
        }
        .accessibilityLabel("Permissions: \(level.label)")
        .accessibilityIdentifier(identifier)
    }

    private var tint: Color { full ? Theme.warning : Theme.textMuted }
}
