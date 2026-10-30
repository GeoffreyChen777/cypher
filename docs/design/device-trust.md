# Device trust

The edge authenticates **accounts**: a WorkOS bearer lets its holder join any
of the user's device rooms. Until device trust, that was also the only thing
standing between a remote caller and a device's full RPC surface — terminals,
file writes, settings, updates. A stolen token or a modified edge Worker could
drive every device of the account.

Device trust adds a layer the edge cannot forge: each device holds its own
keys, the user's devices form a **trust group**, and relayed RPC between group
members runs inside an end-to-end Noise session.

| Piece | Where |
| --- | --- |
| Keys, chain rules, pairing code, Noise sessions | `crates/trust` |
| Relay handshake, sealed frames, refusal | `crates/rpc/src/device_room/{trust,links,relay}.rs` |
| Sync with the edge, pairing flows, `PeerTrust` | `crates/engine/src/device_trust.rs` |
| Relay-served service guard | `crates/engine/src/rpc/relay.rs` |
| Edge storage and validation | `apps/edge/src/trust/` (`TrustRoom`, `t1/{userId}`) |
| iOS port (not yet wired in) | `apps/ios/Cypher/Sync/DeviceTrust.swift` |
| Shared vectors | `protocol/vectors/device-trust-v1.json` |
| UI | Settings → Devices → Pairing (`crates/ui/src/settings/device_pairing.rs`) |

## Keys

Each installation generates an Ed25519 identity key (signs trust entries) and
an X25519 static key (Noise), stored as one `cypher-secrets` secret,
`{device_root}/device-keys.json`: the login Keychain on Apple-signed macOS
builds, a 0600 file otherwise. Keys never leave the device.

## The trust chain

The group is a hash-chained log. Entry 0 is a device adding itself; every
later entry names the previous entry's hash and is signed by a device that is a
member at that point. Entries:

- `add` — a new device's public record;
- `remove` — a member's record, as the group knew it;
- `requirePaired` / `allowUnpaired` — the group's "Only paired devices"
  policy, carrying the signer's own record.

No key may be added twice, so a removed device's keys stay out for good; the
same device id may come back with fresh keys (re-pairing a machine).

The edge (`TrustRoom`) stores the log and validates every append in full, with
compare-and-swap on the sequence number. Devices verify the whole log
themselves and only ever **fast-forward**: a server copy that rewrites an
entry the device already holds is reported as a conflict and never adopted.
The first device to find the group empty founds it; the edge serializes that
race.

The chain lives per signed-in user (`{device_root}/trust/<user>/chain.json`).

## Pairing

1. On a paired device, **Add a device** shows a one-time code: 20 Crockford
   base32 symbols, 100 bits.
2. On the new device, the user enters it. The device posts its public record
   to a mailbox named after the code, MACed with a key derived from it.
3. The member sees "‹name› wants to join" with the new device's key
   fingerprint, and approves. It appends the `add` entry, then posts a
   welcome — the entry's sequence number and hash — MACed the same way.
4. The new device verifies the welcome, fetches the chain, checks that the
   entry at that position adds exactly its own record, and adopts the chain.

The edge relays both messages but never sees the code. It cannot substitute a
device (the join MAC fails) or a device list (the welcome pins the entry
hash, and everything after it is signed by members). 100 bits keep an offline
guess at the MAC key out of reach; a PAKE would allow shorter codes, but
CryptoKit offers no SPAKE2 or CPace primitives for the iOS side.

## Sessions over the relay

`Noise_IK_25519_ChaChaPoly_SHA256`, prologue `cypher-device-room-v1\0‖hostId`.
A client that finds the host in its group sends the first handshake message
(kind `noise`, type byte 1) carrying its own device id; the host answers (type
2) or refuses with a reason (type 3). After that, every RPC frame is sealed
(kind `rpcx`), split into fragments of at most 65519 plaintext bytes
(`flag ‖ ciphertext`, flag 1 while more follow).

- The client knows the host's key from the chain, so a fake host cannot finish
  the handshake.
- The host learns the client's static key in the first message and checks it
  against the chain, refreshing its copy (rate-limited) first.
- Hosts re-check membership on every sealed frame, so a removal ends open
  sessions as soon as the host learns of it.
- A cached plaintext link is replaced once the host's key becomes known.

## "Only paired devices"

A group policy, so it holds for every member at once. With a per-device switch
an open member would relay for an unpaired device into members that refuse
it. When on:

- hosts refuse plaintext clients (each request gets an error, so old clients
  fail fast) and unknown keys at the handshake;
- clients refuse to dial hosts outside their group.

Members re-sync at least once a minute, and a host re-reads the group before
deciding on any new client, so the policy takes effect within about a minute
everywhere and immediately for new connections.

## What the relay-served service refuses

The relay serves `RelayRpc`, not the bare engine service:

- `MethodSpec::local` methods (all device-trust RPCs) answer only for the
  device's own viewports and CLI;
- a call naming a third device in `targetDeviceId` is refused: a relayed
  caller is never passed on under this device's identity.

## What it does not cover

- The policy is off by default, and plaintext stays available while it is off.
  The iPhone app speaks only plaintext until its integration ships (below), so
  turning the policy on cuts the phone off.
- The edge can still delay or drop traffic, withhold a newer chain from a
  device (it cannot forge or roll one back), and see who talks to whom.
- A paired device that is compromised is a member until someone removes it.
- Same-user processes on a device: see [Unix IPC](unix-ipc.md).

## iOS integration (remaining work)

`DeviceTrust.swift` holds the chain rules, the pairing code and MACs, and the
Noise initiator; `scripts/tests/device-trust-vectors.swift` proves them
against the Rust vectors (the macOS stage of `scripts/check.sh`). Still to do
in the app, with Xcode:

1. Generate and keep device keys in the Keychain (`Curve25519.Signing` and
   `Curve25519.KeyAgreement` private keys).
2. Sync the chain (`GET /trust/chain`), verify it, keep it, fast-forward only.
3. Pairing UI: enter a code (or scan a QR code of it), post the join request,
   poll for the welcome, verify, adopt.
4. `DeviceRelayClient`: when the host is in the chain, send the `noise` hello,
   finish on the reply, then seal outgoing `rpc` payloads as `rpcx` fragments
   and open incoming ones; drop any plaintext frame on a secure link.
5. Show the device's key fingerprint while waiting for approval.

Until then the phone works only while the group's policy is off.
