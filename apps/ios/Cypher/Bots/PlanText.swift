// Plan bodies (docs/design/plan-body.md): the reader half of
// crates/proto/src/plan_body.rs. A plan's row keeps its instruction, step
// details and result as previews; when its host had to cut one, the full
// text sits in the workspace chat doc's root map `planBodies[workId]` and the
// row names its rev (`bodyRev` / `resultRev`). `PlanText.of` is the one
// decision every reader makes (R7), with the Rust rules: no rev on the row →
// the row's text; a rev and a body with the same stored rev → the body; a
// rev and no such body → the row's preview, which the page marks as loading.
// A body with no rev on the row is ignored. Clients compare the two strings
// and never hash. Both implementations run protocol/vectors/plan-text-v1.json.

import Foundation
import Loro

/// `planBodies[workId].plan`: the plan's full text, written once by its host.
struct PlanBody: Codable, Hashable, Sendable {
    var rev = ""
    /// A copy of the owner's message.
    var instruction = ""
    /// Every step's full detail by step id.
    var details: [String: String] = [:]
    var at: Int64 = 0

    init(rev: String, instruction: String = "", details: [String: String] = [:], at: Int64 = 0) {
        self.rev = rev
        self.instruction = instruction
        self.details = details
        self.at = at
    }

    /// serde's defaults: a missing field reads as empty, a field of the wrong
    /// type fails the part.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        rev = try c.decode(.rev, or: "")
        instruction = try c.decode(.instruction, or: "")
        details = try c.decode(.details, or: [:])
        at = try c.decode(.at, or: 0)
    }

    /// Whether it is the body a row with `rev` was written with.
    func matches(_ rev: String?) -> Bool {
        guard let rev, !rev.isEmpty else { return false }
        return rev == self.rev
    }
}

/// `planBodies[workId].result`: the full result, when the row's was cut.
struct PlanResult: Codable, Hashable, Sendable {
    var rev = ""
    var text = ""
    var at: Int64 = 0

    init(rev: String, text: String, at: Int64 = 0) {
        self.rev = rev
        self.text = text
        self.at = at
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        rev = try c.decode(.rev, or: "")
        text = try c.decode(.text, or: "")
        at = try c.decode(.at, or: 0)
    }

    func matches(_ rev: String?) -> Bool {
        guard let rev, !rev.isEmpty else { return false }
        return rev == self.rev
    }
}

/// `planBodies[workId]`: either part may be missing (never cut, not written
/// yet, not synced yet, or in a shape this app can't read).
struct WorkBodies: Hashable, Sendable {
    var plan: PlanBody?
    var result: PlanResult?

    init(plan: PlanBody? = nil, result: PlanResult? = nil) {
        self.plan = plan
        self.result = result
    }

    /// An entry as the doc holds it, in JSON form: each part read on its own
    /// (plan_bodies.rs `work_bodies`), so a part that doesn't read costs only
    /// itself; an entry that isn't a map reads as empty.
    init(json: Any?) {
        let parts = json as? [String: Any]
        plan = Self.part(parts?["plan"])
        result = Self.part(parts?["result"])
    }

    init(_ value: LoroValue) {
        self.init(json: value.jsonObject)
    }

    private static func part<T: Decodable>(_ json: Any?) -> T? {
        // Wrapped in an array, so a bare string or number (a malformed part)
        // still serializes; JSONSerialization traps on what isn't JSON.
        guard let json, JSONSerialization.isValidJSONObject([json]),
            let data = try? JSONSerialization.data(withJSONObject: [json])
        else { return nil }
        return (try? JSONDecoder().decode([T].self, from: data))?.first
    }
}

/// Where a `PlanText` part comes from.
enum TextSource: String, Hashable, Sendable {
    /// The row holds the whole text: no rev.
    case row
    /// The full text, from a body whose rev matches the row's.
    case body
    /// The row's preview: cut, its body not here yet (or another rev).
    case preview

    var isPreview: Bool { self == .preview }
}

/// A plan's text as a reader shows it (plan_body.rs `PlanText`).
struct PlanText: Hashable, Sendable {
    var instruction: String
    /// Each step's detail, in the row's step order.
    var details: [String?]
    var plan: TextSource
    var result: String?
    var resultSource: TextSource

    /// `work`'s text with `bodies`, its `planBodies` entry when this device
    /// has it (plan_body.rs `PlanText::of`).
    static func of(_ work: BotWork, _ bodies: WorkBodies?) -> PlanText {
        let planBody = bodies?.plan.flatMap { $0.matches(work.bodyRev) ? $0 : nil }
        let resultBody = bodies?.result.flatMap { $0.matches(work.resultRev) ? $0 : nil }
        let rowDetails = work.steps.map(\.detail)
        var text = PlanText(
            instruction: work.instruction, details: rowDetails, plan: .row,
            result: work.result, resultSource: .row)
        if work.bodyRev != nil {
            if let planBody {
                text.instruction = planBody.instruction
                text.details = work.steps.map { planBody.details[$0.id] ?? $0.detail }
                text.plan = .body
            } else {
                text.plan = .preview
            }
        }
        if work.resultRev != nil {
            if let resultBody {
                text.result = resultBody.text
                text.resultSource = .body
            } else {
                text.resultSource = .preview
            }
        }
        return text
    }

    /// Step `index`'s detail.
    func detail(_ index: Int) -> String? {
        details.indices.contains(index) ? details[index] : nil
    }

    /// Whether any part shows a preview whose body is still to come.
    var isLoading: Bool { plan.isPreview || resultSource.isPreview }
}
