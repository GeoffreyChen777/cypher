// Standalone Foundation runner: tests the iOS device-trust source
// (apps/ios/Cypher/Sync/DeviceTrust.swift) against the shared vectors in
// protocol/vectors/device-trust-v1.json without a simulator. The vectors come
// from the Rust implementation (crates/trust/tests/vectors.rs), so this proves
// the Swift chain rules, pairing MACs and Noise IK initiator match it byte for
// byte.
import CryptoKit
import Foundation

private func hex(_ value: Any?) -> Data {
    TrustHex.decode(value as! String)!
}

private func decode<T: Decodable>(_ type: T.Type, _ value: Any) -> T {
    let data = try! JSONSerialization.data(withJSONObject: value)
    return try! JSONDecoder().decode(type, from: data)
}

@main
struct DeviceTrustVectors {
    static func main() throws {
        let data = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
        let file = try JSONSerialization.jsonObject(with: data) as! [String: Any]

        // Chain encoding, hashing and replay.
        let chainFile = file["chain"] as! [String: Any]
        let rawEntries = chainFile["entries"] as! [[String: Any]]
        let entries = decode([TrustEntry].self, rawEntries)
        for (entry, raw) in zip(entries, rawEntries) {
            precondition(entry.signingBytes == hex(raw["signingBytes"]), "signing bytes of entry \(entry.seq)")
            precondition(entry.hash == hex(raw["hash"]), "hash of entry \(entry.seq)")
        }
        let chain = try TrustChain.verify(entries)
        precondition(chain.members.keys.sorted() == (chainFile["members"] as! [String]), "members")
        precondition(chain.removed.sorted() == (chainFile["removed"] as! [String]), "removed")
        precondition(chain.requirePaired == (chainFile["requirePaired"] as! Bool), "policy")
        precondition(chain.head == hex(rawEntries.last!["hash"]), "head")

        let invalid = file["invalidChains"] as! [[String: Any]]
        for c in invalid {
            let name = c["name"] as! String
            do {
                _ = try TrustChain.verify(decode([TrustEntry].self, c["entries"]!))
                preconditionFailure("\(name) verified")
            } catch let refusal as TrustRefusal {
                precondition(refusal.reason == c["reason"] as! String, "\(name): \(refusal.reason)")
            }
        }

        // Never roll back.
        guard case .same = try chain.compare(entries) else { preconditionFailure("same") }
        guard case .behind = try chain.compare(Array(entries.prefix(2))) else { preconditionFailure("behind") }
        let shorter = try TrustChain.verify(Array(entries.prefix(2)))
        guard case .ahead = try shorter.compare(entries) else { preconditionFailure("ahead") }

        // Devices and fingerprints.
        let devices = file["devices"] as! [[String: Any]]
        for d in devices {
            let pub = decode(TrustDevicePublic.self, d["public"]!)
            precondition(pub.fingerprint == d["fingerprint"] as! String, "fingerprint of \(pub.id)")
            let sign = try Curve25519.Signing.PrivateKey(rawRepresentation: hex(d["signSecret"]))
            let dh = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: hex(d["dhSecret"]))
            precondition(TrustHex.encode(sign.publicKey.rawRepresentation) == pub.signPub, "sign key of \(pub.id)")
            precondition(TrustHex.encode(dh.publicKey.rawRepresentation) == pub.dhPub, "dh key of \(pub.id)")
        }

        // Pairing.
        let pairing = file["pairing"] as! [String: Any]
        let code = PairingCode(pairing["input"] as! String)!
        precondition(code.display == pairing["display"] as! String, "display")
        precondition(code.mailbox == pairing["mailbox"] as! String, "mailbox")
        precondition(PairingCode("0000-0000-0000-0000-000U") == nil && PairingCode("0000") == nil)
        let join = pairing["join"] as! [String: Any]
        let joining = decode(TrustDevicePublic.self, join["device"]!)
        precondition(PairingCode.same(code.joinMac(joining), hex(join["mac"])), "join MAC")
        let welcome = pairing["welcome"] as! [String: Any]
        let welcomeMac = code.welcomeMac(
            seq: (welcome["seq"] as! NSNumber).uint64Value, entry: hex(welcome["entry"]), device: joining)
        precondition(PairingCode.same(welcomeMac, hex(welcome["mac"])), "welcome MAC")
        precondition(!PairingCode.same(PairingCode("ZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZ")!.joinMac(joining), hex(join["mac"])))

        // Noise IK: the client's first message, the host's reply, then
        // transport messages in both directions.
        let noise = file["noise"] as! [String: Any]
        precondition(noise["pattern"] as! String == NoiseInitiator.pattern)
        let hostId = noise["host"] as! String
        precondition(NoiseInitiator.prologue(hostDeviceId: hostId) == hex(noise["prologue"]), "prologue")
        let byId = Dictionary(uniqueKeysWithValues: devices.map { (($0["public"] as! [String: Any])["id"] as! String, $0) })
        let client = byId[noise["client"] as! String]!
        let host = byId[hostId]!
        var initiator = NoiseInitiator(
            local: try .init(rawRepresentation: hex(client["dhSecret"])),
            hostDeviceId: hostId,
            hostKey: try .init(rawRepresentation: hex((host["public"] as! [String: Any])["dhPub"])),
            ephemeral: try .init(rawRepresentation: hex(noise["clientEphemeral"])))
        let first = try initiator.first(localDeviceId: noise["client"] as! String)
        precondition(first == hex(noise["first"]), "first handshake message")
        var session = try initiator.finish(reply: hex(noise["reply"]))

        for m in noise["messages"] as! [[String: Any]] {
            let len = (m["plaintextLen"] as! NSNumber).intValue
            let plaintext = Data((0..<len).map { UInt8($0 % 251) })
            if m["from"] as! String == "client" {
                let fragments = try session.seal(plaintext)
                if let expected = m["fragments"] as? [String] {
                    precondition(fragments.map(TrustHex.encode) == expected, "client message of \(len) bytes")
                } else {
                    let hashes = fragments.map { TrustHex.encode(Data(SHA256.hash(data: $0))) }
                    precondition(hashes == m["fragmentSha256"] as! [String], "client message of \(len) bytes")
                }
            } else {
                var opened: Data?
                for fragment in m["fragments"] as! [String] {
                    opened = try session.open(TrustHex.decode(fragment)!)
                }
                precondition(opened == plaintext, "host message of \(len) bytes")
            }
        }

        // A tampered fragment is refused.
        var replay = NoiseInitiator(
            local: try .init(rawRepresentation: hex(client["dhSecret"])), hostDeviceId: hostId,
            hostKey: try .init(rawRepresentation: hex((host["public"] as! [String: Any])["dhPub"])),
            ephemeral: try .init(rawRepresentation: hex(noise["clientEphemeral"])))
        _ = try replay.first(localDeviceId: noise["client"] as! String)
        var fresh = try replay.finish(reply: hex(noise["reply"]))
        let hostMessage = (noise["messages"] as! [[String: Any]]).first { $0["from"] as! String == "host" }!
        var broken = hex((hostMessage["fragments"] as! [String])[0])
        broken[broken.startIndex + 3] ^= 1
        precondition((try? fresh.open(broken)) == nil, "tampered fragment")

        print("Swift device trust: \(entries.count) entries, \(invalid.count) invalid chains, pairing MACs and the Noise IK transcript match Rust")
    }
}
