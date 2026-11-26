// What a surface shows for a question it answered or a Stop it sent: the
// host's outcome of that very command, read from the doc's ledger, never a
// local guess. One rule for the composer's question panel, the Needs you
// cards and the Asking page; the twin of crates/ui/src/command_outcome.rs,
// pinned by protocol/vectors/command-outcomes-v1.json (CommandOutcomeTests).

import Foundation

/// The host's outcome of one command, as far as this device has seen it.
struct CommandOutcome: Equatable {
    /// The composer's "Sending…" / "Stopping…" wait this long after the
    /// answer or Stop went out, so a host that takes it at once never
    /// flashes the line (composer `OUTCOME_QUIET_MS`).
    static let quietMs = 700

    /// The ledger's status ("pending", "applied", "rejected", "expired",
    /// "superseded"); nil: the command isn't in the doc this device reads yet.
    var status: String?
    var resolution: String?

    /// A question's display from the answer this device sent (nil: none)
    /// and whether its part is resolved.
    static func answer(_ sent: CommandOutcome?, resolved: Bool) -> AnswerOutcome {
        guard let sent else { return resolved ? .done : .open }
        switch sent.status {
        case nil: return resolved ? .done : .sending
        case "pending": return .sending
        case "applied": return resolved ? .done : .sending
        default:
            return .noLongerWaiting(
                "No longer waiting: "
                    + sent.reason(
                        expired: "the answer expired before the session took it",
                        refused: "the session didn't take the answer"))
        }
    }

    /// A Stop's display from its outcome.
    static func stop(_ sent: CommandOutcome) -> StopOutcome {
        switch sent.status {
        case nil, "pending": return .stopping
        case "applied", "superseded": return .done
        default:
            return .refused(
                "Didn't stop: "
                    + sent.reason(
                        expired: "the Stop expired before the session took it",
                        refused: "the session didn't take the Stop"))
        }
    }

    /// The host's words, or the generic phrase for a status without any.
    private func reason(expired: String, refused: String) -> String {
        let words = resolution?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !words.isEmpty { return words }
        return status == "expired" ? expired : refused
    }
}

/// An answer or Stop this device queued, as its ledger command.
enum SentCommand: Equatable {
    case answer(requestId: String, commandId: String)
    case stop(commandId: String)
}

/// A question as the surface that may have answered it shows it.
enum AnswerOutcome: Equatable {
    /// Nothing sent from here: the question with its answers.
    case open
    /// On its way, or applied while the resolution syncs back.
    case sending
    /// Answered (here or elsewhere): the item leaves.
    case done
    /// The host turned the answer down; the line says why.
    case noLongerWaiting(String)

    /// The line in place of the answers, if any.
    var text: String? {
        switch self {
        case .sending: "Sending…"
        case .noLongerWaiting(let text): text
        case .open, .done: nil
        }
    }
}

/// A Stop as the surface that sent it shows it.
enum StopOutcome: Equatable {
    case stopping
    /// Applied, or superseded (its turn already ended): the run's own state
    /// says the rest.
    case done
    /// The host turned it down; the line says why.
    case refused(String)

    var text: String? {
        switch self {
        case .stopping: "Stopping…"
        case .refused(let text): text
        case .done: nil
        }
    }
}
