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
| iOS (joining, chain sync, client sessions) | `apps/ios/Cypher/Sync/{DeviceTrust,DeviceTrustStore,DeviceRelayClient}.swift`, Account › Paired devices |
| Shared vectors | `protocol/vectors/device-trust-v1.json` |
| UI | Settings → Devices → Pairing (`crates/ui/src/settings/device_pairing.rs`) |
| CLI (headless devices) | `cypher pair`, `pair join`, `pair add` (`apps/cypher/src/pair_cli.rs`) |

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

A headless device takes either role from its own CLI over local IPC:
`cypher pair join <code>` and `cypher pair add`, which asks on the terminal
before approving ([Linux setup](../features/linux-setup.md#pairing)).

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
  An iPhone app from before device trust speaks only plaintext, so turning the
  policy on cuts such a phone off until it is updated and paired.
- The edge can still delay or drop traffic, withhold a newer chain from a
  device (it cannot forge or roll one back), and see who talks to whom.
- A paired device that is compromised is a member until someone removes it.
- Same-user processes on a device: see [Unix IPC](unix-ipc.md).

## iOS

The phone only joins: it never founds the group, shows codes or approves
devices. `DeviceTrustStore` keeps its keys in the Keychain (this device only),
syncs the chain every minute (fast-forward only) and runs the joining side of
pairing from Account › Paired devices. `DeviceRelayClient` asks it for the
host's key: with one it runs the Noise handshake and seals every frame through
a single ordered outbox (Noise counts messages, so fragments must leave in
sealing order); without one it refuses under the group's policy. A plaintext
link opened before pairing is replaced on the next call.

Checks:

- `DeviceTrustTests` (CypherTests): the shared vectors, joining, a swapped
  device list, rollback, removal and re-pairing against an in-memory edge.
- `scripts/tests/device-trust-vectors.swift`: the vectors without a simulator
  (macOS stage of `scripts/check.sh`).
- End to end, Development builds only: launch the Dev app with
  `-dev-trust-interop` and `CYPHER_DEV_PAIRING_CODE` (a code from a desktop's
  `StartDevicePairing`) and `CYPHER_DEV_TRUST_HOST` (that desktop's device id)
  in its environment (`SIMCTL_CHILD_` prefix with `simctl launch`), against a
  local `wrangler dev`. Approve on the desktop; the app joins, then calls the
  desktop over a sealed session and writes
  `Documents/development-trust-interop.json`.
