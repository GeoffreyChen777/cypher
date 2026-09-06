// Shared-wire mirrors and pure aggregation, ported from
// crates/ui/src/subagents.rs. Launch acknowledgements are NOT completion.
import Foundation

enum SubagentMode: String, Codable { case sync, async, message }
enum SubagentRunStatus: String, Codable { case running, done, error }

struct SubagentRun: Codable, Hashable {
    var runId: String
    var toolCallId: String?
    var agent: String
    var model: String?
    var task: String
    var mode: SubagentMode
    var status: SubagentRunStatus
    var progress: String?
    var startedAt: Int64
    var updatedAt: Int64
    var endedAt: Int64?
    var childChatId: String?
}

/// Only the public relation metadata is read. The engine retains/reapplies
/// the child profile; mobile neither displays nor rewrites it.
struct ChildChat: Codable, Hashable {
    var parentChatId: String
    var parentRunId: String
    var agent: String
    var task: String
    var mode: SubagentMode
    var toolCallId: String?
}

struct SubagentCallMetadata: Hashable {
    var agent: String
    var task: String
    var isAsync: Bool
}

enum SubagentPanelStatus: String, CaseIterable {
    case running, starting, stale, done, error

    var inFlight: Bool { self == .running || self == .starting || self == .stale }
    var label: String {
        switch self {
        case .running: return "Running"
        case .starting: return "Starting"
        case .stale: return "Status unavailable"
        case .done: return "Done"
        case .error: return "Failed"
        }
    }
}

struct SubagentPanelEntry: Identifiable, Equatable {
    var id: String
    var toolCallId: String?
    var agent: String
    var task: String
    var model: String?
    var mode: SubagentMode
    var status: SubagentPanelStatus
    var progress: String?
    var startedAt: Int64
    var updatedAt: Int64
    var endedAt: Int64?
    var childChatId: String?
}

struct SubagentCounts {
    var running = 0
    var starting = 0
    var stale = 0
    var done = 0
    var failed = 0
    var total: Int { running + starting + stale + done + failed }
    var summary: String {
        [(running, "running"), (starting, "starting"), (stale, "status unavailable"),
         (done, "done"), (failed, "failed")]
            .filter { $0.0 > 0 }.map { "\($0.0) \($0.1)" }.joined(separator: " · ")
    }
    var compact: String {
        if running > 0 { return "\(running) running" + (failed > 0 ? " · \(failed) failed" : "") }
        if starting > 0 { return "\(starting) starting" }
        if stale > 0 { return "\(stale) unavailable" }
        if failed > 0 { return "\(failed) failed" }
        return "\(done) subagents"
    }
}

enum SubagentProjection {
    static let staleMs: Int64 = 45_000

    static func decode<T: Decodable>(_ value: JSONValue?, as type: T.Type) -> T? {
        guard let value, let data = try? JSONEncoder().encode(value) else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }

    static func snapshot(_ value: JSONValue?) -> [SubagentRun] {
        guard case .array(let values) = value else { return [] }
        var seen = Set<String>()
        return values.prefix(32).compactMap {
            guard var run = decode($0, as: SubagentRun.self),
                  !run.runId.isEmpty, !run.agent.isEmpty, seen.insert(run.runId).inserted else { return nil }
            run.agent = String(run.agent.prefix(120))
            run.task = String(run.task.prefix(500))
            run.model = run.model.map { String($0.prefix(256)) }
            run.progress = boundedProgress(run.progress)
            return run
        }
    }

    static func boundedProgress(_ text: String?) -> String? {
        text.map { String($0.suffix(4096)).components(separatedBy: "\n").suffix(8).joined(separator: "\n") }
    }

    static func counts(_ entries: [SubagentPanelEntry]) -> SubagentCounts {
        var result = SubagentCounts()
        for entry in entries {
            switch entry.status {
            case .running: result.running += 1
            case .starting: result.starting += 1
            case .stale: result.stale += 1
            case .done: result.done += 1
            case .error: result.failed += 1
            }
        }
        return result
    }

    static func snapshotStatus(_ run: SubagentRun, now: Int64) -> SubagentPanelStatus {
        switch run.status {
        case .running: return isStale(run.updatedAt, now: now) ? .stale : .running
        case .done: return .done
        case .error: return .error
        }
    }

    private static func isStale(_ updated: Int64, now: Int64) -> Bool {
        // Avoid overflow on malformed timestamps.
        now > updated && Double(now) - Double(updated) > Double(staleMs)
    }

    static func childStatus(_ session: SessionRow, now: Int64) -> SubagentPanelStatus {
        switch session.status {
        case .working, .awaitingInput: return isStale(session.updatedAt, now: now) ? .stale : .running
        case .idle: return .done
        case .errored: return .error
        }
    }

    static func children(of parent: Chat, in chats: [Chat]) -> [Chat] {
        chats.filter {
            $0.id != parent.id && $0.deviceId == parent.deviceId && $0.child?.parentChatId == parent.id
        }
    }

    static func navigableChild(_ entry: SubagentPanelEntry, parent: Chat, chats: [Chat]) -> Chat? {
        children(of: parent, in: chats).first {
            $0.id == entry.childChatId
                && ($0.child?.parentRunId == entry.id
                    || (entry.toolCallId != nil && $0.child?.toolCallId == entry.toolCallId))
        }
    }

    static func aggregate(parent: Chat, transcript: [MessageEntry], snapshot: [SubagentRun],
                          chats: [Chat], sessions: [String: SessionRow], now: Int64) -> [SubagentPanelEntry] {
        var out: [SubagentPanelEntry] = []
        var byTool: [String: Int] = [:]
        for message in transcript {
            for part in message.parts {
                guard case .tool(let id, let call, let isError, let resolved) = part,
                      let info = call.subagent, byTool[id] == nil else { continue }
                let status: SubagentPanelStatus
                if !resolved {
                    guard message.status == .streaming else { continue }
                    status = .starting
                } else if info.isAsync {
                    guard isError else { continue } // success is only a launch ACK
                    status = .error
                } else {
                    status = isError ? .error : .done
                }
                byTool[id] = out.count
                out.append(SubagentPanelEntry(id: id, toolCallId: id, agent: info.agent,
                    task: info.task, mode: info.isAsync ? .async : .sync, status: status,
                    progress: call.progress, startedAt: message.createdAt, updatedAt: message.createdAt))
            }
        }
        var seenRuns = Set<String>()
        for run in snapshot where seenRuns.insert(run.runId).inserted {
            if let tool = run.toolCallId, let index = byTool[tool] {
                var entry = out[index]
                entry.id = run.runId
                entry.model = run.model
                entry.startedAt = min(entry.startedAt, run.startedAt)
                entry.updatedAt = max(entry.updatedAt, run.updatedAt)
                entry.endedAt = run.endedAt
                entry.childChatId = run.childChatId
                entry.progress = entry.progress ?? run.progress
                if entry.mode != .sync || (entry.status != .done && entry.status != .error) {
                    entry.status = snapshotStatus(run, now: now)
                }
                out[index] = entry
            } else {
                out.append(SubagentPanelEntry(id: run.runId, toolCallId: run.toolCallId,
                    agent: run.agent, task: run.task, model: run.model, mode: run.mode,
                    status: snapshotStatus(run, now: now), progress: run.progress,
                    startedAt: run.startedAt, updatedAt: run.updatedAt,
                    endedAt: run.endedAt, childChatId: run.childChatId))
            }
        }
        for chat in children(of: parent, in: chats) {
            guard let child = chat.child else { continue }
            let session = sessions[chat.id].flatMap { $0.deviceId == chat.deviceId ? $0 : nil }
            if let index = out.firstIndex(where: {
                $0.childChatId == chat.id || $0.id == child.parentRunId
                    || (child.toolCallId != nil && $0.toolCallId == child.toolCallId)
            }) {
                out[index].id = child.parentRunId
                out[index].childChatId = chat.id
                out[index].agent = String(child.agent.prefix(120))
                out[index].task = String(child.task.prefix(500))
                out[index].mode = child.mode
                if let session { out[index].status = childStatus(session, now: now) }
            } else {
                out.append(SubagentPanelEntry(id: child.parentRunId, toolCallId: child.toolCallId,
                    agent: String(child.agent.prefix(120)), task: String(child.task.prefix(500)),
                    model: chat.config?.model, mode: child.mode,
                    status: session.map { childStatus($0, now: now) } ?? .starting,
                    startedAt: chat.createdAt, updatedAt: session?.updatedAt ?? chat.createdAt,
                    childChatId: chat.id))
            }
        }
        return out.sorted {
            if $0.status.inFlight != $1.status.inFlight { return $0.status.inFlight }
            if $0.status.inFlight {
                return ($0.startedAt, $0.id) < ($1.startedAt, $1.id)
            }
            let a = $0.endedAt ?? $0.updatedAt, b = $1.endedAt ?? $1.updatedAt
            return a == b ? $0.id < $1.id : a > b
        }
    }
}
