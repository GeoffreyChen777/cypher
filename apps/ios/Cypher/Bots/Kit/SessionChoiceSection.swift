// The model, thinking level and GPT Fast mode of the sessions a workspace
// starts (bot_records.rs `SessionChoice`; the desktop's choice.rs), as the
// routine form and the workspace settings choose them. "Default" leaves a
// field out, so it follows what the footer names.

import SwiftUI

/// A device's models, as a form loads them.
enum ModelChoices: Equatable {
    case loading
    case ready([ModelInfo])
    case unavailable(String)

    var list: [ModelInfo] {
        if case .ready(let list) = self { return list }
        return []
    }
}

struct SessionChoiceSection: View {
    @Binding var choice: SessionChoice
    let models: ModelChoices
    /// What "Default" follows ("Default: the workspace chat's.").
    let fallback: String
    var header: String?
    /// Accessibility identifiers' prefix.
    let id: String

    var body: some View {
        Section {
            Picker(selection: $choice.model) {
                Text("Default").tag(String?.none)
                if let chosen = choice.model, !models.list.contains(where: { $0.id == chosen }) {
                    Text(chosen).tag(String?.some(chosen))
                }
                ForEach(models.list) { info in
                    Text(info.label).tag(String?.some(info.id))
                }
            } label: {
                HStack(spacing: 8) {
                    Text("Model")
                    if models == .loading {
                        ProgressView().controlSize(.small)
                    }
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("\(id)-model")
            Picker("Thinking", selection: $choice.thinking) {
                Text("Default").tag(String?.none)
                if let chosen = choice.thinking, !levels.contains(chosen) {
                    Text(HarnessCatalog.reasoningLabel(chosen)).tag(String?.some(chosen))
                }
                ForEach(levels, id: \.self) { level in
                    Text(HarnessCatalog.reasoningLabel(level)).tag(String?.some(level))
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("\(id)-thinking")
            Picker("Fast mode", selection: $choice.fast) {
                Text("Default").tag(Bool?.none)
                Text("On").tag(Bool?.some(true))
                Text("Off").tag(Bool?.some(false))
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("\(id)-fast")
        } header: {
            if let header { Text(header) }
        } footer: {
            if case .unavailable(let why) = models {
                Text("\(fallback) Fast mode is for GPT models from 5.4 on. \(why)")
            } else {
                Text("\(fallback) Fast mode is for GPT models from 5.4 on.")
            }
        }
        .groupedRowStyle()
    }

    /// The levels the chosen model offers; every level until it is known.
    private var levels: [String] {
        guard let model = choice.model.flatMap({ id in models.list.first { $0.id == id } }),
            !model.reasoningLevels.isEmpty
        else { return SessionChoice.levels }
        return SessionChoice.levels.filter(model.reasoningLevels.contains)
    }
}
