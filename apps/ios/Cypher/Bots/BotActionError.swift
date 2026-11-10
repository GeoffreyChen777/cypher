// Why a workspace action failed, in a short sentence the screens show as is.
// Relay failures are mapped once here (as the desktop's parts.rs
// `clean_error` reads RPC errors), so no screen formats a raw error.

import Foundation

struct BotActionError: LocalizedError, Equatable {
    let message: String

    init(_ message: String) {
        self.message = message
    }

    var errorDescription: String? { message }

    static let alreadyDecided = BotActionError("This plan was already decided.")
    static let stillLive = BotActionError("Stop or cancel this work before deleting it.")
    static let devicesOffline = BotActionError("Your devices are offline.")

    /// A record that is gone (deleted elsewhere, or never synced here).
    static func missing(_ what: String) -> BotActionError {
        BotActionError("This \(what) no longer exists.")
    }

    /// The workspace's host is needed and can't be reached.
    static func hostOffline(_ bot: Bot) -> BotActionError {
        BotActionError("\(bot.name)'s device is offline.")
    }

    /// `error` from a call to `device` (its display name), as a sentence.
    static func from(_ error: Error, device: String) -> BotActionError {
        if let error = error as? BotActionError { return error }
        guard let relay = error as? RelayError else {
            return BotActionError(sentence(error.localizedDescription))
        }
        switch relay {
        case .hostOffline: return BotActionError("\(device) is offline.")
        case .notConnected: return BotActionError("Couldn't reach \(device).")
        case .timeout: return BotActionError("\(device) didn't respond.")
        case .notPaired(let message): return BotActionError(message)
        case .rpc(let message):
            let lower = message.lowercased()
            // An older engine: an unknown RPC, or a `Mutate` op it can't parse.
            if lower.contains("unknown method") || lower.contains("unknown variant") {
                return BotActionError("This needs a newer Cypher on \(device).")
            }
            return BotActionError(sentence(message))
        }
    }

    /// An engine error as a sentence: without the RPC layer's "bad params: "
    /// or "transport: " lead, capitalised.
    static func sentence(_ text: String) -> String {
        var text = text
        for lead in ["bad params: ", "transport: "] where text.hasPrefix(lead) {
            text.removeFirst(lead.count)
        }
        text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let first = text.first else { return "Something went wrong." }
        return first.uppercased() + text.dropFirst()
    }
}
