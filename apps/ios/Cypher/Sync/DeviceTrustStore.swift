// The phone's place in the account's device trust group — the joining side
// of crates/engine/src/device_trust.rs. It keeps the phone's keys in the
// Keychain (this device only), keeps the group's chain in step with the edge
// (fast-forward only: a copy that rewrites an entry the phone holds is a
// conflict, never adopted), and joins with a code shown on a paired desktop.
// DeviceRelayClient asks it for each host's Noise key.
//
// The phone never founds the group or approves other devices: desktops do.
// docs/design/device-trust.md describes the protocol.

import CryptoKit
import Foundation
import Observation

/// What the store needs from the edge's TrustRoom (`/trust/*`). Tests
/// substitute an in-memory edge.
protocol TrustEdge: Sendable {
    func chain() async throws -> [TrustEntry]
    func put(mailbox: String, slot: String, body: Data) async throws
    func get(mailbox: String, slot: String) async throws -> Data?
    func delete(mailbox: String) async throws
}

enum TrustEdgeError: LocalizedError {
    case signedOut
    case http(Int)

    var errorDescription: String? {
        switch self {
        case .signedOut: return "Not signed in"
        case .http(let status): return "The server answered \(status)"
        }
    }
}

struct HTTPTrustEdge: TrustEdge {
    let edgeURL: URL
    let token: @Sendable () async -> String?

    private func request(_ method: String, _ path: String, body: Data? = nil) async throws -> (Data, Int) {
        guard let token = await token() else { throw TrustEdgeError.signedOut }
        var request = URLRequest(url: edgeURL.appending(path: "trust/\(path)"))
        request.httpMethod = method
        request.timeoutInterval = 15
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        return (data, (response as? HTTPURLResponse)?.statusCode ?? 0)
    }

    func chain() async throws -> [TrustEntry] {
        struct Chain: Decodable { let entries: [TrustEntry] }
        let (data, status) = try await request("GET", "chain")
        guard status == 200 else { throw TrustEdgeError.http(status) }
        return try JSONDecoder().decode(Chain.self, from: data).entries
    }

    func put(mailbox: String, slot: String, body: Data) async throws {
        let (_, status) = try await request("PUT", "pairing/\(mailbox)/\(slot)", body: body)
        guard status == 200 else { throw TrustEdgeError.http(status) }
    }

    func get(mailbox: String, slot: String) async throws -> Data? {
        let (data, status) = try await request("GET", "pairing/\(mailbox)/\(slot)")
        if status == 404 { return nil }
        guard status == 200 else { throw TrustEdgeError.http(status) }
        return data
    }

    func delete(mailbox: String) async throws {
        _ = try await request("DELETE", "pairing/\(mailbox)")
    }
}

/// The phone's two private keys.
struct TrustKeys {
    let sign: Curve25519.Signing.PrivateKey
    let dh: Curve25519.KeyAgreement.PrivateKey

    func record(id: String, name: String) -> TrustDevicePublic {
        TrustDevicePublic(
            id: id, name: name,
            signPub: TrustHex.encode(sign.publicKey.rawRepresentation),
            dhPub: TrustHex.encode(dh.publicKey.rawRepresentation))
    }
}

/// Where the phone keeps its keys. Injectable so tests don't touch the
/// Keychain.
protocol TrustKeyStore: Sendable {
    func load(deviceId: String) -> TrustKeys?
    func save(_ keys: TrustKeys, deviceId: String)
}

struct KeychainTrustKeyStore: TrustKeyStore {
    private struct Stored: Codable {
        let deviceId: String
        let sign: String
        let dh: String
    }

    func load(deviceId: String) -> TrustKeys? {
        guard let text = Keychain.load(key: "deviceTrustKeys"),
            let stored = try? JSONDecoder().decode(Stored.self, from: Data(text.utf8)),
            stored.deviceId == deviceId,
            let sign = TrustHex.decode(stored.sign).flatMap({
                try? Curve25519.Signing.PrivateKey(rawRepresentation: $0)
            }),
            let dh = TrustHex.decode(stored.dh).flatMap({
                try? Curve25519.KeyAgreement.PrivateKey(rawRepresentation: $0)
            })
        else { return nil }
        return TrustKeys(sign: sign, dh: dh)
    }

    func save(_ keys: TrustKeys, deviceId: String) {
        let stored = Stored(
            deviceId: deviceId,
            sign: TrustHex.encode(keys.sign.rawRepresentation),
            dh: TrustHex.encode(keys.dh.rawRepresentation))
        guard let data = try? JSONEncoder().encode(stored) else { return }
        Keychain.save(String(decoding: data, as: UTF8.self), key: "deviceTrustKeys", thisDeviceOnly: true)
    }
}

@MainActor
@Observable
final class DeviceTrustStore {
    enum Phase: Equatable {
        /// Not synced with the server yet.
        case unknown
        /// The account has a group and this phone isn't in it.
        case unpaired
        case paired
        /// This phone's keys were removed from the group.
        case removed
    }

    static let pollInterval: Duration = .seconds(2)
    static let joinTimeout: Duration = .seconds(15 * 60)

    private(set) var phase: Phase = .unknown
    private(set) var members: [TrustDevicePublic] = []
    private(set) var requirePaired = false
    private(set) var joining = false
    private(set) var conflict = false
    var error: String?

    let deviceId: String
    let deviceName: String
    private let edge: TrustEdge
    private let keyStore: TrustKeyStore
    private let chainURL: URL?
    @ObservationIgnored private(set) var keys: TrustKeys
    @ObservationIgnored private var chain = TrustChain()
    @ObservationIgnored private var groupExists = false
    @ObservationIgnored private var syncTask: Task<Void, Never>?
    @ObservationIgnored private var joinTask: Task<Void, Never>?

    init(
        deviceId: String, deviceName: String, userId: String, edge: TrustEdge,
        keyStore: TrustKeyStore = KeychainTrustKeyStore(), chainDirectory: URL? = DeviceTrustStore.defaultDirectory
    ) {
        self.deviceId = deviceId
        self.deviceName = deviceName
        self.edge = edge
        self.keyStore = keyStore
        let safeUser = String(userId.map { $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" ? $0 : "_" })
        chainURL = chainDirectory?.appending(path: "\(safeUser).json")
        if let stored = keyStore.load(deviceId: deviceId) {
            keys = stored
        } else {
            keys = TrustKeys(sign: .init(), dh: .init())
            keyStore.save(keys, deviceId: deviceId)
        }
        if let chainURL, let data = try? Data(contentsOf: chainURL),
            let entries = try? JSONDecoder().decode([TrustEntry].self, from: data),
            let stored = try? TrustChain.verify(entries)
        {
            chain = stored
        }
        project()
    }

    static var defaultDirectory: URL? {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appending(path: "trust")
    }

    var fingerprint: String {
        keys.record(id: deviceId, name: deviceName).fingerprint
    }

    /// This phone's Noise static key.
    var localKey: Curve25519.KeyAgreement.PrivateKey { keys.dh }

    /// `deviceId`'s Noise key when it is in the group — and this phone is too:
    /// only a chain the phone belongs to was authenticated by pairing.
    func hostKey(for deviceId: String) -> Curve25519.KeyAgreement.PublicKey? {
        guard isPaired(in: chain), let member = chain.members[deviceId],
            let raw = TrustHex.decode(member.dhPub)
        else { return nil }
        return try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: raw)
    }

    private func isPaired(in chain: TrustChain) -> Bool {
        chain.members[deviceId]?.dhPub == TrustHex.encode(keys.dh.publicKey.rawRepresentation)
    }

    private func project() {
        let paired = isPaired(in: chain)
        phase =
            paired
            ? .paired
            : !chain.entries.isEmpty
                ? .removed
                : groupExists ? .unpaired : .unknown
        members = paired ? chain.members.values.sorted { $0.name < $1.name } : []
        requirePaired = paired && chain.requirePaired
    }

    private func adopt(_ next: TrustChain) {
        chain = next
        groupExists = true
        conflict = false
        if let chainURL, let data = try? JSONEncoder().encode(next.entries) {
            try? FileManager.default.createDirectory(
                at: chainURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? data.write(to: chainURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        }
        project()
    }

    /// Sync now and every minute while the app runs.
    func start() {
        guard syncTask == nil else { return }
        syncTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.sync()
                try? await Task.sleep(for: .seconds(60))
            }
        }
    }

    func stop() {
        syncTask?.cancel()
        syncTask = nil
        joinTask?.cancel()
        joinTask = nil
    }

    func sync() async {
        do {
            let remote = try await edge.chain()
            if !chain.entries.isEmpty {
                switch try chain.compare(remote) {
                case .same, .behind:
                    // A server that lost entries is repaired by the desktops.
                    break
                case .ahead(let next):
                    adopt(next)
                }
            } else if let verified = try? TrustChain.verify(remote), isPaired(in: verified) {
                adopt(verified)
            } else {
                groupExists = !remote.isEmpty
                project()
            }
            if error != nil, !joining { error = nil }
        } catch let refusal as TrustRefusal where refusal.reason == "diverged" {
            conflict = true
        } catch {
            if phase == .unknown { self.error = "Couldn't reach the server: \(error.localizedDescription)" }
        }
    }

    // MARK: Joining

    /// Ask to join with the code a paired desktop shows; the desktop approves.
    func join(code input: String) {
        guard let code = PairingCode(input) else {
            error = "That code isn't valid. It has 20 letters and digits, like ABCD-EFGH-JKMN-PQRS-TVWX."
            return
        }
        guard phase != .paired else {
            error = "This iPhone is already paired."
            return
        }
        joinTask?.cancel()
        error = nil
        joining = true
        joinTask = Task { [weak self] in
            guard let self else { return }
            do {
                try await self.runJoin(code)
            } catch is CancellationError {
            } catch {
                self.error = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
            }
            self.joining = false
            try? await self.edge.delete(mailbox: code.mailbox)
        }
    }

    func cancelJoin() {
        joinTask?.cancel()
        joinTask = nil
        joining = false
    }

    private struct JoinFailure: LocalizedError {
        let errorDescription: String?
        init(_ message: String) { errorDescription = message }
    }

    private struct JoinBody: Encodable {
        let device: TrustDevicePublic
        let mac: String
    }

    private struct WelcomeBody: Decodable {
        let seq: UInt64
        let entry: String
        let mac: String
    }

    private func runJoin(_ code: PairingCode) async throws {
        // Keys the group has seen before (this phone was removed) can't be
        // added again: make new ones.
        let seen = try await edge.chain()
        let current = keys.record(id: deviceId, name: deviceName)
        if seen.contains(where: { $0.device.signPub == current.signPub || $0.device.dhPub == current.dhPub }) {
            keys = TrustKeys(sign: .init(), dh: .init())
            keyStore.save(keys, deviceId: deviceId)
            chain = TrustChain()
            groupExists = true
            if let chainURL { try? FileManager.default.removeItem(at: chainURL) }
            project()
        }
        let me = keys.record(id: deviceId, name: deviceName)
        let body = try JSONEncoder().encode(JoinBody(device: me, mac: TrustHex.encode(code.joinMac(me))))
        try await edge.put(mailbox: code.mailbox, slot: "join", body: body)

        let clock = ContinuousClock()
        let deadline = clock.now + Self.joinTimeout
        var welcome: WelcomeBody?
        while welcome == nil {
            guard clock.now < deadline else {
                throw JoinFailure("No approval arrived. Show a new code on your Mac or PC.")
            }
            try await Task.sleep(for: Self.pollInterval)
            if let data = try? await edge.get(mailbox: code.mailbox, slot: "welcome") {
                welcome = try? JSONDecoder().decode(WelcomeBody.self, from: data)
            }
        }
        guard let welcome, let entryHash = TrustHex.decode(welcome.entry), let mac = TrustHex.decode(welcome.mac),
            PairingCode.same(code.welcomeMac(seq: welcome.seq, entry: entryHash, device: me), mac)
        else { throw JoinFailure("The approval failed verification. Pair again with a new code.") }
        let verified: TrustChain
        do {
            verified = try TrustChain.verify(try await edge.chain())
        } catch let refusal as TrustRefusal {
            throw JoinFailure("The server's device list is invalid (\(refusal.reason)).")
        }
        // The welcome vouches for the log up to the entry adding this phone;
        // everything after it is signed by members.
        let index = Int(welcome.seq)
        guard index < verified.entries.count, verified.entries[index].hash == entryHash,
            verified.entries[index].kind == "add", verified.entries[index].device == me
        else { throw JoinFailure("The server's device list doesn't match the approval.") }
        guard isPaired(in: verified) else {
            throw JoinFailure("This iPhone was removed right after being added.")
        }
        adopt(verified)
    }
}
