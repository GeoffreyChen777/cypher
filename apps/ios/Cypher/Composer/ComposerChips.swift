// Composer picker chips: the generic chip and the model chip.

import SwiftUI

// MARK: - Composer chip

/// The composer's picker trigger chip: optional brand mark, label, an
/// optional quieter detail, chevron. The model chip carries the thinking
/// level as its detail ("Claude Opus 5 · High"), so one chip opens both.
struct ComposerChip: View {
    let label: String
    var detail: String?
    var badgeHarness: String?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                if let badgeHarness {
                    HarnessBadge(harness: badgeHarness, size: 15)
                }
                Text(label)
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.text.opacity(0.9))
                    .lineLimit(1)
                if let detail {
                    Text("·")
                        .font(Theme.sans(13))
                        .foregroundStyle(Theme.textFaint)
                    Text(detail)
                        .font(Theme.sans(13, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textFaint)
            }
            .padding(.horizontal, 13)
            .frame(height: 40)
            .background(whiteAlpha(0.08), in: Capsule())
            .overlay(Capsule().strokeBorder(whiteAlpha(0.08), lineWidth: 1))
        }
        .buttonStyle(ChipPressButtonStyle())
    }
}

/// The one model chip both composers show: the provider's mark, the model
/// and, when it takes one, its thinking level.
struct ModelChip: View {
    let model: ModelInfo?
    /// Shown while no catalog model matches (a configured id, or nothing).
    var fallbackLabel = "Select model"
    let reasoning: String?
    let action: () -> Void

    var body: some View {
        ComposerChip(label: model?.label ?? fallbackLabel,
                     detail: reasoning.map(HarnessCatalog.reasoningLabel),
                     badgeHarness: model.flatMap {
                         HarnessCatalog.providerBadgeHarness(HarnessCatalog.providerId(of: $0.id))
                     },
                     action: action)
            .accessibilityIdentifier("model-chip")
    }
}
