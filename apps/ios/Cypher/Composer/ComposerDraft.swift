// Composer draft: the text plus an editing generation, so late callbacks from
// a retired editor never touch a newer draft.

import Observation
import SwiftUI

/// Each accepted send ends an editing generation. Late callbacks from the
/// retired native editor must not restore its old text or erase a new draft.
@MainActor
@Observable
final class ComposerDraft {
    var text = ""
    private(set) var revision = 0

    var binding: Binding<String> {
        let generation = revision
        return Binding(
            get: { self.text },
            set: { value in
                guard self.revision == generation else { return }
                self.text = value
            }
        )
    }

    func clearAfterSend() {
        revision += 1
        text = ""
    }

    /// Bumped by `replace`, so the editor moves its caret to the end.
    private(set) var caretRequest = 0

    /// Rewrite the whole draft from outside the editor (a picked slash
    /// command) — same editor, caret after the new text.
    func replace(with value: String) {
        text = value
        caretRequest += 1
    }
}
