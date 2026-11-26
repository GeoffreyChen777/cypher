import XCTest

@testable import Cypher

/// Git refs are found as the desktop finds them (crates/proto/src/git_refs.rs):
/// both run protocol/vectors/git-refs-v1.json.
final class GitRefsTests: XCTestCase {
    func testSharedVectors() throws {
        let root = try TestSupport.repoRoot()
        let data = try Data(contentsOf: root.appendingPathComponent("protocol/vectors/git-refs-v1.json"))
        let file = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let rows = try XCTUnwrap(file["detect"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(rows.count, 30)
        for row in rows {
            let name = try XCTUnwrap(row["name"] as? String)
            let text = try XCTUnwrap(row["text"] as? String)
            let inCode = try XCTUnwrap(row["inCode"] as? Bool)
            let found = GitRefs.find(text, inCode: inCode).map { match -> [String: String] in
                var value = ["match": match.text(in: text)]
                switch match.ref {
                case .issue(let repo, let number):
                    value["kind"] = "issue"
                    value["number"] = "\(number)"
                    if let repo { value["repo"] = repo }
                case .commit(let repo, let sha):
                    value["kind"] = "commit"
                    value["sha"] = sha
                    if let repo { value["repo"] = repo }
                }
                return value
            }
            let expect = try XCTUnwrap(row["expect"] as? [[String: Any]]).map { $0.mapValues { "\($0)" } }
            XCTAssertEqual(found, expect, name)
            for match in GitRefs.find(text, inCode: inCode) { XCTAssertTrue(match.ref.isValid, name) }
        }
    }

    func testResolutionsDecode() throws {
        let pull = try JSONDecoder().decode(
            GitRefResolution.self,
            from: Data(
                #"{"kind":"pullRequest","repo":"o/r","number":3,"title":"T","state":"draft","author":"me","headBranch":"topic","baseBranch":"main","url":"https://github.com/o/r/pull/3","checks":{"state":"pending","passed":1,"failed":0,"pending":2,"skipped":0}}"#
                    .utf8))
        guard case .pullRequest(let preview) = pull else { return XCTFail("\(pull)") }
        XCTAssertEqual(preview.state, .draft)
        XCTAssertEqual(preview.checks?.pending, 2)
        XCTAssertTrue(pull.hasCard)
        let newer = try JSONDecoder().decode(
            GitRefResolution.self, from: Data(#"{"kind":"unavailable","reason":"quota"}"#.utf8))
        XCTAssertFalse(newer.hasCard)
        XCTAssertNil(newer.url)
    }

    @MainActor func testDecorationMarksProseAndWholeCodeSpans() {
        let text = TranscriptTextStyle.inline([
            InlineRun(text: "See #12 and ", style: .plain),
            InlineRun(text: "a1b2c3d", style: InlineStyle(code: true)),
            InlineRun(text: " but not ", style: .plain),
            InlineRun(text: "git show a1b2c3d", style: InlineStyle(code: true)),
        ])
        let refs = GitRefText.refs(in: GitRefText.decorate(text))
        XCTAssertEqual(refs, [.issue(repo: nil, number: 12), .commit(repo: nil, sha: "a1b2c3d")])
    }

    func testCardText() {
        XCTAssertEqual(
            GitRefCardText.checks(GitChecksSummary(state: .failure, passed: 3, failed: 1, pending: 0, skipped: 0)),
            "Checks failed: 3 passed · 1 failed")
        XCTAssertEqual(GitRefCardText.size(files: 1, plus: 4, minus: 2), "1 file · +4 −2")
        XCTAssertEqual(GitRefCardText.age(1_000, now: Date(timeIntervalSince1970: 1_000 + 7200)), "2 h ago")
    }
}
