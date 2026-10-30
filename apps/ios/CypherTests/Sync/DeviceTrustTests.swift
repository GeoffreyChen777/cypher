// Device trust on the iPhone: the shared vectors in
// protocol/vectors/device-trust-v1.json (run by the Rust and TypeScript
// implementations too, protocol/README.md), and DeviceTrustStore's joining
// flow against an in-memory edge — including an edge that swaps the device
// list.

import CryptoKit
import XCTest

@testable import Cypher

private func vectors() throws -> [String: Any] {
    let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/device-trust-v1.json")
    return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
}

private func decode<T: Decodable>(_ type: T.Type, _ value: Any?) throws -> T {
    try JSONDecoder().decode(type, from: JSONSerialization.data(withJSONObject: try XCTUnwrap(value)))
}

/// The edge's TrustRoom in memory. Appends are validated like the real one.
private actor FakeTrustEdge: TrustEdge {
    var entries: [TrustEntry] = []
    var mailboxes: [String: Data] = [:]

    func chain() async throws -> [TrustEntry] { entries }
    func put(mailbox: String, slot: String, body: Data) async throws { mailboxes["\(mailbox)/\(slot)"] = body }
    func get(mailbox: String, slot: String) async throws -> Data? { mailboxes["\(mailbox)/\(slot)"] }
    func delete(mailbox: String) async throws { mailboxes = mailboxes.filter { !$0.key.hasPrefix(mailbox) } }

    func replace(_ entries: [TrustEntry]) { self.entries = entries }
    func append(_ entry: TrustEntry) throws {
        _ = try TrustChain.verify(entries + [entry])
        entries.append(entry)
    }
}

private final class MemoryKeyStore: TrustKeyStore, @unchecked Sendable {
    private var keys: [String: TrustKeys] = [:]
    func load(deviceId: String) -> TrustKeys? { keys[deviceId] }
    func save(_ keys: TrustKeys, deviceId: String) { self.keys[deviceId] = keys }
}

/// A desktop in the test: signs entries the way crates/trust does.
private struct Desktop {
    let id: String
    let keys = TrustKeys(sign: .init(), dh: .init())
    var record: TrustDevicePublic { keys.record(id: id, name: "Desktop \(id)") }

    func sign(seq: Int, after chain: [TrustEntry], kind: String, device: TrustDevicePublic) throws -> TrustEntry {
        let prev = chain.last.map { TrustHex.encode($0.hash) } ?? String(repeating: "0", count: 64)
        let unsigned = TrustEntry(
            seq: UInt64(seq), prev: prev, kind: kind, device: device, at: 1, signer: id,
            sig: String(repeating: "0", count: 128))
        let signature = try keys.sign.signature(for: unsigned.signingBytes)
        return TrustEntry(
            seq: unsigned.seq, prev: prev, kind: kind, device: device, at: 1, signer: id,
            sig: TrustHex.encode(signature))
    }
}

final class DeviceTrustVectorTests: XCTestCase {
    func testSharedChainVectors() throws {
        let file = try vectors()
        let chain = try XCTUnwrap(file["chain"] as? [String: Any])
        let entries = try decode([TrustEntry].self, chain["entries"])
        let raw = try XCTUnwrap(chain["entries"] as? [[String: Any]])
        for (entry, raw) in zip(entries, raw) {
            XCTAssertEqual(TrustHex.encode(entry.signingBytes), raw["signingBytes"] as? String)
            XCTAssertEqual(TrustHex.encode(entry.hash), raw["hash"] as? String)
        }
        let verified = try TrustChain.verify(entries)
        XCTAssertEqual(verified.members.keys.sorted(), chain["members"] as? [String])
        XCTAssertEqual(verified.requirePaired, chain["requirePaired"] as? Bool)
        for invalid in try XCTUnwrap(file["invalidChains"] as? [[String: Any]]) {
            let name = invalid["name"] as? String ?? ""
            XCTAssertThrowsError(try TrustChain.verify(decode([TrustEntry].self, invalid["entries"])), name) {
                XCTAssertEqual(($0 as? TrustRefusal)?.reason, invalid["reason"] as? String, name)
            }
        }
    }

    func testSharedPairingAndNoiseVectors() throws {
        let file = try vectors()
        let pairing = try XCTUnwrap(file["pairing"] as? [String: Any])
        let code = try XCTUnwrap(PairingCode(try XCTUnwrap(pairing["input"] as? String)))
        XCTAssertEqual(code.display, pairing["display"] as? String)
        XCTAssertEqual(code.mailbox, pairing["mailbox"] as? String)
        let join = try XCTUnwrap(pairing["join"] as? [String: Any])
        let device = try decode(TrustDevicePublic.self, join["device"])
        XCTAssertEqual(TrustHex.encode(code.joinMac(device)), join["mac"] as? String)

        let noise = try XCTUnwrap(file["noise"] as? [String: Any])
        let devices = try XCTUnwrap(file["devices"] as? [[String: Any]])
        func vectorDevice(_ id: String) throws -> [String: Any] {
            try XCTUnwrap(devices.first { ($0["public"] as? [String: Any])?["id"] as? String == id })
        }
        let client = try vectorDevice(try XCTUnwrap(noise["client"] as? String))
        let host = try vectorDevice(try XCTUnwrap(noise["host"] as? String))
        var initiator = NoiseInitiator(
            local: try .init(rawRepresentation: XCTUnwrap(TrustHex.decode(client["dhSecret"] as? String ?? ""))),
            hostDeviceId: try XCTUnwrap(noise["host"] as? String),
            hostKey: try .init(
                rawRepresentation: XCTUnwrap(
                    TrustHex.decode((host["public"] as? [String: Any])?["dhPub"] as? String ?? ""))),
            ephemeral: try .init(
                rawRepresentation: XCTUnwrap(TrustHex.decode(noise["clientEphemeral"] as? String ?? ""))))
        let first = try initiator.first(localDeviceId: try XCTUnwrap(noise["client"] as? String))
        XCTAssertEqual(TrustHex.encode(first), noise["first"] as? String)
        XCTAssertNoThrow(try initiator.finish(reply: XCTUnwrap(TrustHex.decode(noise["reply"] as? String ?? ""))))
    }

    func testRefusalsReadLikeTheDesktop() {
        XCTAssertTrue(DeviceRelayClient.refusalMessage("untrusted_device").contains("isn't paired"))
        XCTAssertTrue(DeviceRelayClient.refusalMessage("other").contains("(other)"))
    }
}

@MainActor
final class DeviceTrustStoreTests: XCTestCase {
    private var directory: URL!

    override func setUp() async throws {
        directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: directory)
    }

    private func store(_ edge: FakeTrustEdge, keys: MemoryKeyStore = MemoryKeyStore()) -> DeviceTrustStore {
        DeviceTrustStore(
            deviceId: "ios-1", deviceName: "iPhone", userId: "user-1", edge: edge,
            keyStore: keys, chainDirectory: directory)
    }

    /// The desktop's side of pairing: wait for the join, approve it.
    private func approve(on edge: FakeTrustEdge, desktop: Desktop, code: PairingCode) async throws -> TrustEntry {
        var request: Data?
        for _ in 0..<200 where request == nil {
            request = try await edge.get(mailbox: code.mailbox, slot: "join")
            if request == nil { try await Task.sleep(for: .milliseconds(20)) }
        }
        struct Join: Decodable {
            let device: TrustDevicePublic
            let mac: String
        }
        let join = try JSONDecoder().decode(Join.self, from: XCTUnwrap(request))
        XCTAssertEqual(TrustHex.encode(code.joinMac(join.device)), join.mac)
        let chain = try await edge.chain()
        let add = try desktop.sign(seq: chain.count, after: chain, kind: "add", device: join.device)
        try await edge.append(add)
        let welcome: [String: Any] = [
            "seq": add.seq, "entry": TrustHex.encode(add.hash),
            "mac": TrustHex.encode(code.welcomeMac(seq: add.seq, entry: add.hash, device: join.device)),
        ]
        try await edge.put(
            mailbox: code.mailbox, slot: "welcome", body: JSONSerialization.data(withJSONObject: welcome))
        return add
    }

    private func founded(by desktop: Desktop) async throws -> FakeTrustEdge {
        let edge = FakeTrustEdge()
        try await edge.append(desktop.sign(seq: 0, after: [], kind: "add", device: desktop.record))
        return edge
    }

    private func waitUntil(_ condition: @MainActor () -> Bool) async throws {
        for _ in 0..<500 where !condition() { try await Task.sleep(for: .milliseconds(20)) }
        XCTAssertTrue(condition())
    }

    func testJoiningWithACodeMakesTheDesktopsTrusted() async throws {
        let desktop = Desktop(id: "mac-1")
        let edge = try await founded(by: desktop)
        let trust = store(edge)
        await trust.sync()
        XCTAssertEqual(trust.phase, .unpaired)
        XCTAssertNil(trust.hostKey(for: "mac-1"))

        let code = try XCTUnwrap(PairingCode("ABCD-EFGH-JKMN-PQRS-TVWX"))
        trust.join(code: code.display)
        XCTAssertTrue(trust.joining)
        _ = try await approve(on: edge, desktop: desktop, code: code)
        try await waitUntil { trust.phase == .paired }
        XCTAssertFalse(trust.joining)
        XCTAssertEqual(trust.hostKey(for: "mac-1")?.rawRepresentation, desktop.keys.dh.publicKey.rawRepresentation)
        XCTAssertEqual(trust.members.map(\.id).sorted(), ["ios-1", "mac-1"])

        // A new policy from the desktop reaches the phone on its next sync.
        let chain = try await edge.chain()
        try await edge.append(
            desktop.sign(seq: chain.count, after: chain, kind: "requirePaired", device: desktop.record))
        await trust.sync()
        XCTAssertTrue(trust.requirePaired)
    }

    func testAJoinerRefusesADeviceListTheDesktopDidNotApprove() async throws {
        let desktop = Desktop(id: "mac-1")
        let edge = try await founded(by: desktop)
        let trust = store(edge)
        await trust.sync()
        let code = try XCTUnwrap(PairingCode("ABCD-EFGH-JKMN-PQRS-TVWX"))
        trust.join(code: code.display)
        _ = try await approve(on: edge, desktop: desktop, code: code)
        // Before the phone fetches it, the edge swaps in its own group that
        // also "adds" the phone.
        let attacker = Desktop(id: "attacker")
        let genesis = try attacker.sign(seq: 0, after: [], kind: "add", device: attacker.record)
        let phone = trust.keys.record(id: "ios-1", name: "iPhone")
        let fake = [genesis, try attacker.sign(seq: 1, after: [genesis], kind: "add", device: phone)]
        await edge.replace(fake)
        try await waitUntil { !trust.joining }
        XCTAssertNotEqual(trust.phase, .paired)
        XCTAssertNotNil(trust.error)
        XCTAssertNil(trust.hostKey(for: "attacker"))
    }

    func testTheChainNeverRollsBackAndSurvivesARestart() async throws {
        let desktop = Desktop(id: "mac-1")
        let edge = try await founded(by: desktop)
        let keys = MemoryKeyStore()
        let trust = store(edge, keys: keys)
        await trust.sync()
        let code = try XCTUnwrap(PairingCode("ABCD-EFGH-JKMN-PQRS-TVWX"))
        trust.join(code: code.display)
        _ = try await approve(on: edge, desktop: desktop, code: code)
        try await waitUntil { trust.phase == .paired }

        let attacker = Desktop(id: "attacker")
        await edge.replace([try attacker.sign(seq: 0, after: [], kind: "add", device: attacker.record)])
        await trust.sync()
        XCTAssertTrue(trust.conflict)
        XCTAssertEqual(trust.phase, .paired)
        XCTAssertNil(trust.hostKey(for: "attacker"))

        let reopened = store(edge, keys: keys)
        XCTAssertEqual(reopened.phase, .paired)
        XCTAssertNotNil(reopened.hostKey(for: "mac-1"))
    }

    func testARemovedPhoneRejoinsWithNewKeys() async throws {
        let desktop = Desktop(id: "mac-1")
        let edge = try await founded(by: desktop)
        let trust = store(edge)
        await trust.sync()
        var code = try XCTUnwrap(PairingCode("ABCD-EFGH-JKMN-PQRS-TVWX"))
        trust.join(code: code.display)
        let added = try await approve(on: edge, desktop: desktop, code: code)
        try await waitUntil { trust.phase == .paired }

        var chain = try await edge.chain()
        try await edge.append(desktop.sign(seq: chain.count, after: chain, kind: "remove", device: added.device))
        await trust.sync()
        XCTAssertEqual(trust.phase, .removed)
        XCTAssertNil(trust.hostKey(for: "mac-1"))

        let oldKey = trust.keys.dh.publicKey.rawRepresentation
        code = try XCTUnwrap(PairingCode("ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZ1"))
        trust.join(code: code.display)
        _ = try await approve(on: edge, desktop: desktop, code: code)
        try await waitUntil { trust.phase == .paired }
        XCTAssertNotEqual(trust.keys.dh.publicKey.rawRepresentation, oldKey)
        chain = try await edge.chain()
        XCTAssertEqual(chain.last?.device.dhPub, TrustHex.encode(trust.keys.dh.publicKey.rawRepresentation))
    }

    func testCodesAreCheckedBeforeAnythingIsSent() async throws {
        let edge = FakeTrustEdge()
        let trust = store(edge)
        trust.join(code: "not a code")
        XCTAssertFalse(trust.joining)
        XCTAssertNotNil(trust.error)
        let sent = await edge.mailboxes
        XCTAssertTrue(sent.isEmpty)
    }
}
