// What a surface shows for an answer or Stop it sent, on the shared vectors in
// protocol/vectors/command-outcomes-v1.json (crates/ui/src/command_outcome.rs
// runs the same file).

import XCTest

@testable import Cypher

final class CommandOutcomeTests: XCTestCase {
    private func vectors() throws -> [String: Any] {
        let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/command-outcomes-v1.json")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }

    private func state(_ outcome: AnswerOutcome) -> String {
        switch outcome {
        case .open: "open"
        case .sending: "sending"
        case .done: "done"
        case .noLongerWaiting: "noLongerWaiting"
        }
    }

    private func state(_ outcome: StopOutcome) -> String {
        switch outcome {
        case .stopping: "stopping"
        case .done: "done"
        case .refused: "refused"
        }
    }

    func testAnswerVectors() throws {
        let cases = try XCTUnwrap(try vectors()["answer"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 15)
        for vector in cases {
            let name = try XCTUnwrap(vector["name"] as? String)
            let sent = try XCTUnwrap(vector["sent"] as? Bool)
            let command = CommandOutcome(
                status: vector["status"] as? String, resolution: vector["resolution"] as? String)
            let outcome = CommandOutcome.answer(
                sent ? command : nil, resolved: try XCTUnwrap(vector["resolved"] as? Bool))
            let expect = try XCTUnwrap(vector["expect"] as? [String: Any])
            XCTAssertEqual(state(outcome), expect["state"] as? String, name)
            XCTAssertEqual(outcome.text, expect["text"] as? String, name)
        }
    }

    func testStopVectors() throws {
        let cases = try XCTUnwrap(try vectors()["stop"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 8)
        for vector in cases {
            let name = try XCTUnwrap(vector["name"] as? String)
            let outcome = CommandOutcome.stop(
                CommandOutcome(status: vector["status"] as? String, resolution: vector["resolution"] as? String))
            let expect = try XCTUnwrap(vector["expect"] as? [String: Any])
            XCTAssertEqual(state(outcome), expect["state"] as? String, name)
            XCTAssertEqual(outcome.text, expect["text"] as? String, name)
        }
    }
}
