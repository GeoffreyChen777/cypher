// Model picker sheet (provider rail + model list) and the shared picker row.

import SwiftUI

// MARK: - Model picker sheet

/// One card for both choices. Providers sit in a rail across the top (the
/// desktop picker's grouping: a flat list of every gateway model was
/// unreadable on a phone), the viewed provider's models in a card below,
/// and the chosen model's thinking levels pinned along the bottom, so they
/// stay in reach however long the list. The rail opens on the current
/// model's provider.
///
/// Every pick reports the model and level together: a model that doesn't
/// take the current level gets its default, written in the same change.
struct ModelPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    /// Empty means unavailable, never a static fallback.
    let models: [ModelInfo]
    let modelId: String
    let reasoning: String?
    var loading = false
    var onRefresh: (() -> Void)?
    let onSelect: (_ modelId: String, _ reasoning: String?) -> Void

    /// The provider whose models the list shows; nil = the current model's.
    @State private var pickedProvider: String?
    @Namespace private var thinkingSelection

    private var groups: [HarnessCatalog.ProviderGroup] { HarnessCatalog.providerGroups(models) }
    private var current: ModelInfo? { models.first { $0.id == modelId } }

    private var viewedProvider: String? {
        if let pickedProvider, groups.contains(where: { $0.id == pickedProvider }) { return pickedProvider }
        let provider = HarnessCatalog.providerId(of: modelId)
        if groups.contains(where: { $0.id == provider }) { return provider }
        return groups.first?.id
    }

    private var viewedGroup: HarnessCatalog.ProviderGroup? {
        groups.first { $0.id == viewedProvider }
    }

    /// The level in effect for the current model: the chosen one if it
    /// takes it, else its default.
    private var currentLevel: String? {
        guard let current else { return nil }
        if let reasoning, current.reasoningLevels.contains(reasoning) { return reasoning }
        return HarnessCatalog.defaultReasoning(for: current)
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if loading {
                        // Centered in the sheet's visible area, not tucked
                        // into the list's top-left corner.
                        VStack(spacing: 10) {
                            ProgressView()
                                .controlSize(.regular)
                                .tint(Theme.textMuted)
                            Text("Loading models…")
                                .font(Theme.sans(13))
                                .foregroundStyle(Theme.textMuted)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 72)
                    } else if groups.isEmpty {
                        Text("No models loaded. Close this picker and retry from the session.")
                            .font(Theme.sans(13))
                            .foregroundStyle(Theme.textMuted)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 72)
                            .padding(.horizontal, 12)
                    } else {
                        if groups.count > 1 {
                            providerRail
                        }
                        if let viewedGroup {
                            modelCard(viewedGroup)
                        }
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 8)
                .padding(.bottom, 20)
                // The loading/empty content has a narrow intrinsic width;
                // don't wait for full-width rows to size the sheet.
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                if !loading, let current {
                    thinkingBar(current)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(SheetStyle.panel)
            .navigationTitle("Model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if let onRefresh {
                    ToolbarItem(placement: .cancellationAction) {
                        Button(action: onRefresh) {
                            Image(systemName: "arrow.clockwise")
                                .font(.system(size: 13, weight: .semibold))
                        }
                        .disabled(loading)
                        .accessibilityLabel("Refresh")
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 13, weight: .semibold))
                    }
                    .accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }

    // MARK: Providers

    /// The provider rail: one chip per provider in catalog order. The viewed
    /// chip is the filled high-contrast pill; the others carry a faint count.
    private var providerRail: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(groups) { group in
                    providerChip(group, viewed: group.id == viewedProvider)
                }
            }
            .padding(.horizontal, 2)
        }
        .scrollClipDisabled()
        .accessibilityIdentifier("model-provider-rail")
    }

    private func providerChip(_ group: HarnessCatalog.ProviderGroup, viewed: Bool) -> some View {
        Button {
            UISelectionFeedbackGenerator().selectionChanged()
            withAnimation(Motion.collapse) { pickedProvider = group.id }
        } label: {
            HStack(spacing: 6) {
                if let badge = HarnessCatalog.providerBadgeHarness(group.id) {
                    HarnessBadge(harness: badge, size: 14, neutral: viewed ? Theme.bg : Theme.text)
                } else {
                    Image(systemName: "globe")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(viewed ? Theme.bg : Theme.textMuted)
                }
                Text(group.name)
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(viewed ? Theme.bg : Theme.text)
                    .lineLimit(1)
                Text("\(group.models.count)")
                    .font(Theme.sans(11))
                    .foregroundStyle(viewed ? Theme.bg.opacity(0.65) : Theme.textFaint)
            }
            .padding(.horizontal, 12)
            .frame(height: 36)
            .background(
                viewed ? AnyShapeStyle(Theme.text) : AnyShapeStyle(whiteAlpha(0.06)),
                in: Capsule()
            )
            .overlay(Capsule().strokeBorder(whiteAlpha(viewed ? 0 : 0.08), lineWidth: 1))
            .contentShape(Capsule())
        }
        .buttonStyle(ChipPressButtonStyle())
        .accessibilityIdentifier("model-provider-\(group.id)")
        .accessibilityAddTraits(viewed ? .isSelected : [])
    }

    // MARK: Models

    /// The viewed provider's models in one card, a line each: the name, its
    /// context size, and a check on the current one. With a single provider
    /// there's no rail, so the card is headed by its name.
    private func modelCard(_ group: HarnessCatalog.ProviderGroup) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if groups.count == 1 {
                SheetLabel(group.name)
            }
            VStack(spacing: 0) {
                ForEach(Array(group.models.enumerated()), id: \.element.id) { ix, m in
                    if ix > 0 {
                        Rectangle()
                            .fill(SheetStyle.rowSeparator)
                            .frame(height: 1)
                            .padding(.leading, 16)
                    }
                    modelRow(m)
                }
            }
            .background(SheetStyle.cardFill, in: RoundedRectangle(cornerRadius: SheetStyle.cardRadius))
            .clipShape(RoundedRectangle(cornerRadius: SheetStyle.cardRadius))
            .overlay(
                RoundedRectangle(cornerRadius: SheetStyle.cardRadius)
                    .strokeBorder(whiteAlpha(0.06), lineWidth: 1))
        }
    }

    private func modelRow(_ m: ModelInfo) -> some View {
        let selected = m.id == modelId
        return Button {
            UISelectionFeedbackGenerator().selectionChanged()
            onSelect(m.id, Self.level(keeping: reasoning, on: m))
        } label: {
            HStack(spacing: 10) {
                Text(m.label)
                    .font(Theme.sans(15, weight: selected ? .semibold : .medium))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Spacer(minLength: 8)
                if let context = HarnessCatalog.contextLabel(m) {
                    Text(context)
                        .font(Theme.sans(12.5))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
                Image(systemName: "checkmark")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.text)
                    .frame(width: 16)
                    .opacity(selected ? 1 : 0)
            }
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity, minHeight: 50, alignment: .leading)
            .background(selected ? whiteAlpha(0.06) : .clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(SheetRowButtonStyle())
        .accessibilityIdentifier("model-row-\(m.id)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    /// The level a newly picked model runs at: the current one if it takes
    /// it, else its default (nil for a model without levels).
    static func level(keeping reasoning: String?, on model: ModelInfo) -> String? {
        if let reasoning, model.reasoningLevels.contains(reasoning) { return reasoning }
        return HarnessCatalog.defaultReasoning(for: model)
    }

    // MARK: Thinking

    /// Pinned under the list: the current model's thinking levels as one
    /// segmented track, and a line on what the chosen level does.
    private func thinkingBar(_ model: ModelInfo) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                SheetLabel("Thinking")
                Spacer(minLength: 8)
                if let currentLevel, let hint = HarnessCatalog.reasoningHint(currentLevel) {
                    Text(hint)
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
            }
            if model.reasoningLevels.isEmpty {
                Text("\(model.label) doesn't take a thinking level.")
                    .font(Theme.sans(13))
                    .foregroundStyle(Theme.textMuted)
                    .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
                    .padding(.horizontal, 4)
            } else {
                HStack(spacing: 2) {
                    ForEach(model.reasoningLevels, id: \.self) { level in
                        thinkingSegment(level, model: model)
                    }
                }
                .padding(3)
                .background(whiteAlpha(0.06), in: Capsule())
                .motionAnimation(Motion.collapse, value: currentLevel)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("thinking-levels")
            }
        }
        .padding(.horizontal, 20)
        .padding(.top, 14)
        .padding(.bottom, 10)
        .background(SheetStyle.panel)
        .overlay(alignment: .top) {
            Rectangle().fill(SheetStyle.rowSeparator).frame(height: 1)
        }
    }

    private func thinkingSegment(_ level: String, model: ModelInfo) -> some View {
        let selected = level == currentLevel
        return Button {
            guard !selected else { return }
            UISelectionFeedbackGenerator().selectionChanged()
            onSelect(model.id, level)
        } label: {
            Text(HarnessCatalog.reasoningLabel(level))
                .font(Theme.sans(13, weight: .medium))
                .foregroundStyle(selected ? Theme.bg : Theme.text.opacity(0.85))
                .lineLimit(1)
                .minimumScaleFactor(0.75)
                .frame(maxWidth: .infinity, minHeight: 36)
                .background {
                    if selected {
                        Capsule()
                            .fill(Theme.text)
                            .matchedGeometryEffect(id: "level", in: thinkingSelection)
                    }
                }
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("thinking-\(level)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

// MARK: - Shared picker row

/// t3's ModelRow — the ONE row style every picker sheet uses (model, trait,
/// ref, checkout): the selected row is a filled high-contrast pill with a
/// trailing checkmark; unselected rows sit almost flat on the sheet. Optional
/// leading line icon and a busy spinner for rows whose pick runs async (git
/// checkouts).
struct PickRow: View {
    let title: String
    var subtitle: String?
    var icon: LineIcon?
    var busy = false
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                if let icon {
                    LineIconView(
                        icon, size: 15,
                        color: selected ? Theme.bg : Theme.textMuted
                    )
                    .frame(width: 20)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(Theme.sans(15, weight: .medium))
                        .foregroundStyle(selected ? Theme.bg : Theme.text)
                    if let subtitle {
                        Text(subtitle)
                            .font(Theme.sans(12))
                            .foregroundStyle(selected ? Theme.bg.opacity(0.65) : Theme.textMuted)
                    }
                }
                Spacer(minLength: 8)
                if busy {
                    ProgressView()
                        .controlSize(.small)
                        .tint(selected ? Theme.bg : Theme.textMuted)
                } else {
                    Image(systemName: "checkmark")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.bg)
                        .opacity(selected ? 1 : 0)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                selected ? AnyShapeStyle(Theme.text) : AnyShapeStyle(whiteAlpha(0.03)),
                in: RoundedRectangle(cornerRadius: 12)
            )
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(SheetRowButtonStyle())
    }
}
