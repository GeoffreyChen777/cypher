// Device trust: the iOS port of crates/trust — the trust chain rules, the
// pairing code and its MACs, and the client (initiator) half of the
// Noise_IK_25519_ChaChaPoly_SHA256 sessions the device relay runs between
// paired devices. The Rust, TypeScript and Swift implementations all run
// protocol/vectors/device-trust-v1.json (protocol/README.md).
//
// The phone is only ever a client, so it needs no responder. Not yet wired
// into DeviceRelayClient: see docs/design/device-trust.md for the steps.

import CryptoKit
import Foundation

enum TrustHex {
    static func encode(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    static func decode(_ text: String) -> Data? {
        let chars = Array(text.utf8)
        guard chars.count % 2 == 0 else { return nil }
        var out = Data(capacity: chars.count / 2)
        var i = 0
        while i < chars.count {
            guard let hi = nibble(chars[i]), let lo = nibble(chars[i + 1]) else { return nil }
            out.append(hi << 4 | lo)
            i += 2
        }
        return out
    }

    private static func nibble(_ c: UInt8) -> UInt8? {
        switch c {
        case 0x30...0x39: return c - 0x30
        case 0x61...0x66: return c - 0x61 + 10
        case 0x41...0x46: return c - 0x41 + 10
        default: return nil
        }
    }
}

struct TrustDevicePublic: Codable, Equatable {
    let id: String
    let name: String
    /// Ed25519, hex.
    let signPub: String
    /// X25519 (the Noise static key), hex.
    let dhPub: String

    /// `str id ‖ str name ‖ signPub ‖ dhPub`.
    var encoded: Data {
        var out = Data()
        TrustBytes.put(id, into: &out)
        TrustBytes.put(name, into: &out)
        out.append(TrustHex.decode(signPub) ?? Data())
        out.append(TrustHex.decode(dhPub) ?? Data())
        return out
    }

    var fingerprint: String {
        TrustHex.encode(Data(SHA256.hash(data: TrustHex.decode(signPub) ?? Data()).prefix(8)))
    }
}

struct TrustEntry: Codable, Equatable {
    let seq: UInt64
    let prev: String
    /// `add` | `remove` | `requirePaired` | `allowUnpaired`.
    let kind: String
    let device: TrustDevicePublic
    let at: UInt64
    let signer: String
    let sig: String

    static let kindBytes: [String: UInt8] = ["add": 1, "remove": 2, "requirePaired": 3, "allowUnpaired": 4]

    var signingBytes: Data {
        var out = Data("cypher-trust-entry-v1\u{0}".utf8)
        TrustBytes.put(seq, into: &out)
        out.append(TrustHex.decode(prev) ?? Data())
        out.append(Self.kindBytes[kind] ?? 0)
        out.append(device.encoded)
        TrustBytes.put(at, into: &out)
        TrustBytes.put(signer, into: &out)
        return out
    }

    var hash: Data {
        var data = Data("cypher-trust-hash-v1\u{0}".utf8)
        data.append(signingBytes)
        data.append(TrustHex.decode(sig) ?? Data())
        return Data(SHA256.hash(data: data))
    }
}

enum TrustBytes {
    static func put(_ value: String, into out: inout Data) {
        let utf8 = Data(value.utf8)
        withUnsafeBytes(of: UInt32(utf8.count).bigEndian) { out.append(contentsOf: $0) }
        out.append(utf8)
    }

    static func put(_ value: UInt64, into out: inout Data) {
        withUnsafeBytes(of: value.bigEndian) { out.append(contentsOf: $0) }
    }
}

/// Why a trust chain was refused; the reasons match crates/trust.
struct TrustRefusal: Error, Equatable {
    let reason: String
}

/// A verified trust chain and the membership it yields.
struct TrustChain {
    static let maxEntries = 1024
    private static let maxField = 128

    private(set) var entries: [TrustEntry] = []
    private(set) var members: [String: TrustDevicePublic] = [:]
    private(set) var removed: Set<String> = []
    private(set) var requirePaired = false
    private var keysSeen: Set<String> = []

    var head: Data? { entries.last?.hash }

    static func verify(_ entries: [TrustEntry]) throws -> TrustChain {
        guard entries.count <= maxEntries else { throw TrustRefusal(reason: "too many entries") }
        var chain = TrustChain()
        for entry in entries { try chain.apply(entry) }
        return chain
    }

    private static func validField(_ value: String) -> Bool {
        let bytes = value.utf8.count
        return bytes > 0 && bytes <= maxField
            && !value.unicodeScalars.contains { $0.value < 0x20 || (0x7F...0x9F).contains($0.value) }
    }

    private static func validKey(_ hex: String) -> Bool {
        hex.utf8.count == 64 && TrustHex.decode(hex) != nil
    }

    mutating func apply(_ entry: TrustEntry) throws {
        func bad(_ reason: String) -> TrustRefusal { TrustRefusal(reason: reason) }
        guard entry.seq == UInt64(entries.count) else { throw bad("out of sequence") }
        guard TrustHex.decode(entry.prev) == (head ?? Data(count: 32)) else {
            throw bad("does not follow the previous entry")
        }
        guard Self.validField(entry.signer) else { throw bad("bad signer") }
        guard Self.validField(entry.device.id) else { throw bad("bad device id") }
        guard Self.validField(entry.device.name) else { throw bad("bad device name") }
        guard Self.validKey(entry.device.signPub), Self.validKey(entry.device.dhPub),
            let signKey = TrustHex.decode(entry.device.signPub),
            (try? Curve25519.Signing.PublicKey(rawRepresentation: signKey)) != nil
        else { throw bad("bad signing key") }
        guard TrustEntry.kindBytes[entry.kind] != nil, let sig = TrustHex.decode(entry.sig), sig.count == 64 else {
            throw bad("malformed entry")
        }
        let signer: TrustDevicePublic
        if entry.seq == 0 {
            guard entry.kind == "add", entry.signer == entry.device.id else {
                throw bad("the first entry must be a device adding itself")
            }
            signer = entry.device
        } else {
            guard let member = members[entry.signer] else { throw bad("signed by a device outside the group") }
            signer = member
        }
        guard let signerKey = TrustHex.decode(signer.signPub),
            let key = try? Curve25519.Signing.PublicKey(rawRepresentation: signerKey),
            key.isValidSignature(sig, for: entry.signingBytes)
        else { throw bad("bad signature") }
        switch entry.kind {
        case "add":
            guard members[entry.device.id] == nil else { throw bad("device already a member") }
            guard !keysSeen.contains(entry.device.signPub), !keysSeen.contains(entry.device.dhPub) else {
                throw bad("key already used")
            }
            keysSeen.insert(entry.device.signPub)
            keysSeen.insert(entry.device.dhPub)
            members[entry.device.id] = entry.device
        case "remove":
            guard members[entry.device.id] == entry.device else {
                throw bad("removes a device that is not a member")
            }
            members[entry.device.id] = nil
            removed.insert(entry.device.id)
        default:
            guard entry.device == signer else { throw bad("a policy entry must carry its signer's record") }
            requirePaired = entry.kind == "requirePaired"
        }
        entries.append(entry)
    }

    /// How a chain from the server relates to this one. Never rolls back.
    enum Comparison {
        case same
        case ahead(TrustChain)
        case behind
    }

    func compare(_ remote: [TrustEntry]) throws -> Comparison {
        let shared = min(remote.count, entries.count)
        guard Array(remote.prefix(shared)) == Array(entries.prefix(shared)) else {
            throw TrustRefusal(reason: "diverged")
        }
        if remote.count == entries.count { return .same }
        if remote.count < entries.count { return .behind }
        return .ahead(try TrustChain.verify(remote))
    }
}

/// A one-time pairing code: 20 Crockford base32 symbols (100 bits).
struct PairingCode: Equatable {
    private static let alphabet = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ")
    let normalized: String

    /// Accept what a person typed: any case, dashes and spaces, O for 0 and
    /// I or L for 1.
    init?(_ input: String) {
        var code = ""
        for c in input.uppercased() {
            switch c {
            case "-", " ": continue
            case "O": code.append("0")
            case "I", "L": code.append("1")
            default:
                guard Self.alphabet.contains(c) else { return nil }
                code.append(c)
            }
        }
        guard code.count == 20 else { return nil }
        normalized = code
    }

    var display: String {
        stride(from: 0, to: 20, by: 4).map { i in
            let start = normalized.index(normalized.startIndex, offsetBy: i)
            return String(normalized[start..<normalized.index(start, offsetBy: 4)])
        }.joined(separator: "-")
    }

    private func derive(_ info: String, _ count: Int) -> SymmetricKey {
        HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: Data(normalized.utf8)),
            salt: Data("cypher-pairing-v1".utf8), info: Data(info.utf8), outputByteCount: count)
    }

    var mailbox: String {
        derive("mailbox", 16).withUnsafeBytes { TrustHex.encode(Data($0)) }
    }

    private func mac(_ label: String, _ parts: [Data]) -> Data {
        var hmac = HMAC<SHA256>(key: derive("mac", 32))
        hmac.update(data: Data(label.utf8))
        for part in parts { hmac.update(data: part) }
        return Data(hmac.finalize())
    }

    func joinMac(_ device: TrustDevicePublic) -> Data {
        mac("join\u{0}", [device.encoded])
    }

    func welcomeMac(seq: UInt64, entry: Data, device: TrustDevicePublic) -> Data {
        var seqBytes = Data()
        TrustBytes.put(seq, into: &seqBytes)
        return mac("welcome\u{0}", [seqBytes, entry, device.encoded])
    }

    /// Constant-time comparison against a received MAC.
    static func same(_ a: Data, _ b: Data) -> Bool {
        a.count == b.count && zip(a, b).reduce(UInt8(0)) { $0 | ($1.0 ^ $1.1) } == 0
    }
}

// MARK: - Noise IK, initiator side

enum NoiseError: Error { case malformed, decrypt, tooLarge }

struct NoiseCipher {
    var key: SymmetricKey?
    var n: UInt64 = 0

    private func nonce() throws -> ChaChaPoly.Nonce {
        var bytes = Data(count: 4)
        withUnsafeBytes(of: n.littleEndian) { bytes.append(contentsOf: $0) }
        return try ChaChaPoly.Nonce(data: bytes)
    }

    mutating func encrypt(ad: Data, _ plaintext: Data) throws -> Data {
        guard let key else { return plaintext }
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: nonce(), authenticating: ad)
        n += 1
        return box.ciphertext + box.tag
    }

    mutating func decrypt(ad: Data, _ ciphertext: Data) throws -> Data {
        guard let key else { return ciphertext }
        guard ciphertext.count >= 16 else { throw NoiseError.decrypt }
        let box = try ChaChaPoly.SealedBox(
            nonce: nonce(), ciphertext: Data(ciphertext.dropLast(16)), tag: Data(ciphertext.suffix(16)))
        guard let plaintext = try? ChaChaPoly.open(box, using: key, authenticating: ad) else {
            throw NoiseError.decrypt
        }
        n += 1
        return plaintext
    }
}

private struct NoiseSymmetric {
    var ck: Data
    var h: Data
    var cipher = NoiseCipher()

    init(protocolName: String) {
        let name = Data(protocolName.utf8)
        h = name.count <= 32 ? name + Data(count: 32 - name.count) : Data(SHA256.hash(data: name))
        ck = h
    }

    mutating func mixHash(_ data: Data) {
        h = Data(SHA256.hash(data: h + data))
    }

    /// Noise's HKDF with two outputs is RFC 5869 with empty info.
    static func hkdf(_ ck: Data, _ ikm: Data) -> (Data, Data) {
        let out = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: ikm), salt: ck, info: Data(), outputByteCount: 64
        ).withUnsafeBytes { Data($0) }
        return (Data(out.prefix(32)), Data(out.suffix(32)))
    }

    mutating func mixKey(_ ikm: Data) {
        let (ck, key) = Self.hkdf(ck, ikm)
        self.ck = ck
        cipher = NoiseCipher(key: SymmetricKey(data: key))
    }

    mutating func encryptAndHash(_ plaintext: Data) throws -> Data {
        let ciphertext = try cipher.encrypt(ad: h, plaintext)
        mixHash(ciphertext)
        return ciphertext
    }

    mutating func decryptAndHash(_ ciphertext: Data) throws -> Data {
        let plaintext = try cipher.decrypt(ad: h, ciphertext)
        mixHash(ciphertext)
        return plaintext
    }

    func split() -> (NoiseCipher, NoiseCipher) {
        let (first, second) = Self.hkdf(ck, Data())
        return (NoiseCipher(key: SymmetricKey(data: first)), NoiseCipher(key: SymmetricKey(data: second)))
    }
}

private func dh(_ priv: Curve25519.KeyAgreement.PrivateKey, _ pub: Curve25519.KeyAgreement.PublicKey) throws -> Data {
    try priv.sharedSecretFromKeyAgreement(with: pub).withUnsafeBytes { Data($0) }
}

/// The client half of a device-room handshake (crates/trust/src/noise.rs).
struct NoiseInitiator {
    static let pattern = "Noise_IK_25519_ChaChaPoly_SHA256"

    private var state: NoiseSymmetric
    private let local: Curve25519.KeyAgreement.PrivateKey
    private let ephemeral: Curve25519.KeyAgreement.PrivateKey
    private let hostKey: Curve25519.KeyAgreement.PublicKey

    static func prologue(hostDeviceId: String) -> Data {
        Data("cypher-device-room-v1\u{0}".utf8) + Data(hostDeviceId.utf8)
    }

    init(
        local: Curve25519.KeyAgreement.PrivateKey, hostDeviceId: String,
        hostKey: Curve25519.KeyAgreement.PublicKey,
        ephemeral: Curve25519.KeyAgreement.PrivateKey = .init()
    ) {
        self.local = local
        self.ephemeral = ephemeral
        self.hostKey = hostKey
        state = NoiseSymmetric(protocolName: Self.pattern)
        state.mixHash(Self.prologue(hostDeviceId: hostDeviceId))
        state.mixHash(hostKey.rawRepresentation)
    }

    /// `-> e, es, s, ss` with this device's id as the payload.
    mutating func first(localDeviceId: String) throws -> Data {
        var out = ephemeral.publicKey.rawRepresentation
        state.mixHash(ephemeral.publicKey.rawRepresentation)
        state.mixKey(try dh(ephemeral, hostKey))
        out += try state.encryptAndHash(local.publicKey.rawRepresentation)
        state.mixKey(try dh(local, hostKey))
        out += try state.encryptAndHash(Data(localDeviceId.utf8))
        return out
    }

    /// `<- e, ee, se`, then transport mode.
    mutating func finish(reply: Data) throws -> NoiseSession {
        guard reply.count >= 48,
            let remoteEphemeral = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: Data(reply.prefix(32)))
        else { throw NoiseError.malformed }
        state.mixHash(remoteEphemeral.rawRepresentation)
        state.mixKey(try dh(ephemeral, remoteEphemeral))
        state.mixKey(try dh(local, remoteEphemeral))
        _ = try state.decryptAndHash(Data(reply.dropFirst(32)))
        let (send, receive) = state.split()
        return NoiseSession(send: send, receive: receive)
    }
}

/// An established session: sealed RPC frames, fragmented like the Rust side.
struct NoiseSession {
    static let fragment = 65_535 - 16
    static let maxMessage = 32 * 1024 * 1024

    var send: NoiseCipher
    var receive: NoiseCipher
    private var inbox = Data()

    init(send: NoiseCipher, receive: NoiseCipher) {
        self.send = send
        self.receive = receive
    }

    /// One message → fragments `flag ‖ ciphertext`, flag 1 while more follow.
    mutating func seal(_ message: Data) throws -> [Data] {
        guard message.count <= Self.maxMessage else { throw NoiseError.tooLarge }
        let starts = message.isEmpty ? [0] : Array(stride(from: 0, to: message.count, by: Self.fragment))
        return try starts.enumerated().map { index, start in
            let chunk =
                message.isEmpty
                ? Data()
                : Data(
                    message[message.startIndex + start..<message.startIndex + min(start + Self.fragment, message.count)]
                )
            let flag: UInt8 = index == starts.count - 1 ? 0 : 1
            return Data([flag]) + (try send.encrypt(ad: Data(), chunk))
        }
    }

    /// One fragment; the whole message once its last fragment arrives. Any
    /// error leaves the session unusable.
    mutating func open(_ fragment: Data) throws -> Data? {
        guard let flag = fragment.first, flag <= 1 else { throw NoiseError.malformed }
        let plaintext = try receive.decrypt(ad: Data(), Data(fragment.dropFirst()))
        guard inbox.count + plaintext.count <= Self.maxMessage else { throw NoiseError.tooLarge }
        inbox.append(plaintext)
        guard flag == 0 else { return nil }
        defer { inbox = Data() }
        return inbox
    }
}
