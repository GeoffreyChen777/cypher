// `SlashCommandText` on the shared vectors in
// protocol/vectors/slash-commands-v1.json (crates/ui/src/slash_command.rs
// runs the same file), and the send gates that read it.

import XCTest

@testable import Cypher

final class SlashCommandTextTests: XCTestCase {
    func testSharedVectors() throws {
        let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/slash-commands-v1.json")
        let root = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        XCTAssertEqual(root["version"] as? Int, 1)
        let cases = try XCTUnwrap(root["cases"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 30)
        for vector in cases {
            let name = try XCTUnwrap(vector["name"] as? String)
            let text = try XCTUnwrap(vector["text"] as? String)
            let known = (vector["commands"] as? [String]).map(Set.init)
            let expect = vector["expect"] as? String
            XCTAssertEqual(SlashCommandText.command(in: text, known: known), expect, name)
        }
    }

    func testCommentsBlockOnlyRealCommands() {
        XCTAssertTrue(CommentPrompt.blocksSlash("/goal ship", hasComments: true))
        XCTAssertFalse(CommentPrompt.blocksSlash("/Users/me/x.png 是什么", hasComments: true))
        XCTAssertFalse(CommentPrompt.blocksSlash("/ 这个呢", hasComments: true))
        XCTAssertFalse(CommentPrompt.blocksSlash("/notacommand x", hasComments: true, known: ["goal"]))
        XCTAssertTrue(CommentPrompt.blocksSlash("/goal x", hasComments: true, known: ["goal"]))
    }
}
