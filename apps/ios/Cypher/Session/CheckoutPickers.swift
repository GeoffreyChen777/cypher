// Ref and checkout pickers for the new-session sheet.

import SwiftUI

// MARK: - Ref picker sheet

/// Base-ref selector (the desktop footer's branch popover): branch rows with
/// current-checkout / worktree markers. Picks that require a git checkout run
/// inline — a spinner on the row, git's error surfaced in place on failure
/// (dirty tree, held ref), success dismisses.
struct RefPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    let refs: [RepoRef]
    let selected: String?
    /// Returns an error message to keep the sheet open, or nil to close.
    let onPick: (RepoRef) async -> String?

    @State private var switching: String?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    SheetLabel("Ref")
                    if refs.isEmpty {
                        Text("Loading refs from the device…")
                            .font(Theme.sans(13))
                            .foregroundStyle(Theme.textFaint)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 28)
                    } else {
                        ForEach(refs, id: \.name) { ref in
                            row(ref)
                        }
                    }
                    if let error {
                        Text(error)
                            .font(Theme.sans(12.5))
                            .foregroundStyle(Theme.danger)
                            .padding(.horizontal, 4)
                            .padding(.top, 4)
                    }
                }
                .padding(20)
            }
            .background(SheetStyle.panel)
            .navigationTitle("Select ref")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
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
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }

    private func row(_ ref: RepoRef) -> some View {
        PickRow(
            title: ref.name,
            subtitle: subtitle(for: ref),
            busy: switching == ref.name,
            selected: ref.name == selected
        ) {
            guard switching == nil else { return }
            UISelectionFeedbackGenerator().selectionChanged()
            error = nil
            switching = ref.name
            Task { @MainActor in
                let result = await onPick(ref)
                switching = nil
                if let result {
                    error = result
                } else {
                    dismiss()
                }
            }
        }
    }

    private func subtitle(for ref: RepoRef) -> String? {
        if ref.current { return "Current checkout" }
        if ref.worktreePath != nil { return "Checked out in a worktree" }
        return nil
    }
}

// MARK: - Checkout picker sheet

/// Where the session runs (the desktop's checkout popover): the space's
/// folder as-is (or the picked ref's existing worktree), or a fresh isolated
/// worktree created off the base ref on send.
struct CheckoutPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    let kind: CheckoutKind
    let selectedRefHasWorktree: Bool
    let onPick: (CheckoutKind) -> Void

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    SheetLabel("Checkout")
                    row(
                        .local,
                        title: selectedRefHasWorktree ? "Current worktree" : "Current checkout",
                        subtitle: selectedRefHasWorktree
                            ? "Reuse the picked ref's existing worktree"
                            : "Run in the space's folder as-is")
                    row(
                        .newWorktree, title: "New worktree",
                        subtitle: "A fresh isolated worktree created off the picked base ref")
                }
                .padding(20)
            }
            .background(SheetStyle.panel)
            .navigationTitle("Checkout")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
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
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }

    private func row(_ rowKind: CheckoutKind, title: String, subtitle: String) -> some View {
        PickRow(title: title, subtitle: subtitle, selected: rowKind == kind) {
            UISelectionFeedbackGenerator().selectionChanged()
            onPick(rowKind)
            dismiss()
        }
    }
}
