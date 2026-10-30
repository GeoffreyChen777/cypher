//! Builds `protocol/vectors/device-trust-v1.json`, which the edge (TypeScript)
//! and iOS (Swift) implementations run, and fails when the committed file
//! differs. Everything is deterministic: Ed25519 signatures are, and the Noise
//! handshake uses fixed ephemeral keys. Regenerate with
//! `UPDATE_VECTORS=1 cargo test -p cypher-trust --test vectors`.

use cypher_trust::noise::{Accepting, Initiator, PATTERN, prologue};
use cypher_trust::{DeviceKeys, PairingCode, TrustChain, TrustEntry};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn seed(n: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = n.wrapping_mul(31).wrapping_add(i as u8);
    }
    out
}

struct Device {
    keys: DeviceKeys,
    name: &'static str,
    sign: [u8; 32],
    dh: [u8; 32],
}

fn device(id: &str, name: &'static str, n: u8) -> Device {
    let (sign, dh) = (seed(n), seed(n + 100));
    Device {
        keys: DeviceKeys::from_secrets(id.into(), sign, dh),
        name,
        sign,
        dh,
    }
}

fn entry_json(entry: &TrustEntry) -> Value {
    let mut value = serde_json::to_value(entry).unwrap();
    value["signingBytes"] = hex(&entry.signing_bytes()).into();
    value["hash"] = hex(&entry.hash()).into();
    value
}

fn invalid(name: &str, entries: Vec<TrustEntry>) -> Value {
    assert!(TrustChain::verify(entries.clone()).is_err(), "{name}");
    json!({ "name": name, "entries": entries })
}

/// `plaintext[i] = i % 251`.
fn plaintext(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn build() -> Value {
    let a = device("device-a", "Studio Mac", 1);
    let b = device("device-b", "Build Box", 2);
    let c = device("device-c", "iPhone", 3);
    let x = device("device-x", "Stranger", 4);

    // A founds the group, adds B; B adds C; C removes A.
    let mut chain = TrustChain::genesis(&a.keys, a.name, 1_700_000_000_000).unwrap();
    let add_b = chain
        .propose_add(&a.keys, b.keys.public(b.name), 1_700_000_001_000)
        .unwrap();
    chain.append(add_b).unwrap();
    let add_c = chain
        .propose_add(&b.keys, c.keys.public(c.name), 1_700_000_002_000)
        .unwrap();
    chain.append(add_c).unwrap();
    let remove_a = chain
        .propose_remove(&c.keys, "device-a", 1_700_000_003_000)
        .unwrap();
    chain.append(remove_a).unwrap();
    let entries = chain.entries().to_vec();

    let mut edited = entries.clone();
    edited[1].device.name = "Renamed".into();
    let mut relinked = entries.clone();
    relinked[2].prev = [7; 32];
    let mut resequenced = entries[..2].to_vec();
    resequenced[1].seq = 5;
    // Entries an outsider signs, rebuilt so only the signer is wrong.
    let base = TrustChain::verify(entries[..2].to_vec()).unwrap();
    let mut by_outsider = base.propose_add(&a.keys, x.keys.public(x.name), 5).unwrap();
    by_outsider.signer = "device-x".into();
    let self_add = {
        let mut probe = TrustChain::genesis(&x.keys, x.name, 5).unwrap().entries()[0].clone();
        probe.seq = 2;
        probe.prev = base.head().unwrap();
        probe
    };
    let after_removal = TrustChain::verify(entries.clone()).unwrap();
    let readd = {
        let mut entry = after_removal
            .propose_add(&b.keys, x.keys.public(x.name), 6)
            .unwrap();
        entry.device = a.keys.public(a.name);
        entry
    };
    let duplicate_key = {
        let mut twin = x.keys.public(x.name);
        twin.dh_pub = b.keys.public(b.name).dh_pub;
        let mut entry = base.propose_add(&a.keys, x.keys.public(x.name), 6).unwrap();
        entry.device = twin;
        entry
    };
    let mut signed_by_removed = after_removal
        .propose_add(&b.keys, x.keys.public(x.name), 7)
        .unwrap();
    signed_by_removed.signer = "device-a".into();

    // Pairing: C joins the group through A.
    let code = PairingCode::parse("oil0-23ab-cdef-ghjk-mnpq").unwrap();
    let joining = c.keys.public(c.name);
    let join = code.join(&joining);
    let welcome = code.welcome(2, entries[2].hash(), &joining);

    // Noise: A (client) to B (host).
    let host_id = "device-b";
    let client_e = seed(201);
    let host_e = seed(202);
    let (initiator, first) =
        Initiator::start_with(&a.keys, host_id, &b.keys.dh_public(), Some(&client_e)).unwrap();
    let accepting = Accepting::read_with(&b.keys, host_id, &first, Some(&host_e)).unwrap();
    let (reply, mut host) = accepting.accept().unwrap();
    let mut client = initiator.finish(&reply).unwrap();
    let mut messages = Vec::new();
    for (from, len) in [
        ("client", 0usize),
        ("client", 37),
        ("host", 11),
        ("client", 70_000),
    ] {
        let body = plaintext(len);
        let fragments = if from == "client" {
            client.seal(&body).unwrap()
        } else {
            host.seal(&body).unwrap()
        };
        let receiver = if from == "client" {
            &mut host
        } else {
            &mut client
        };
        let mut opened = None;
        for fragment in &fragments {
            opened = receiver.open(fragment).unwrap();
        }
        assert_eq!(opened.as_deref(), Some(&body[..]));
        let mut message = json!({ "from": from, "plaintextLen": len });
        if len > 1000 {
            message["fragmentSha256"] = fragments
                .iter()
                .map(|f| hex(&Sha256::digest(f)))
                .collect::<Vec<_>>()
                .into();
        } else {
            message["fragments"] = fragments.iter().map(|f| hex(f)).collect::<Vec<_>>().into();
        }
        messages.push(message);
    }

    let devices: Vec<Value> = [&a, &b, &c, &x]
        .iter()
        .map(|d| {
            json!({
                "signSecret": hex(&d.sign),
                "dhSecret": hex(&d.dh),
                "public": d.keys.public(d.name),
                "fingerprint": d.keys.public(d.name).fingerprint(),
            })
        })
        .collect();
    json!({
        "comment": "Generated by crates/trust/tests/vectors.rs. Byte strings are lowercase hex; plaintext[i] = i % 251.",
        "devices": devices,
        "chain": {
            "entries": entries.iter().map(entry_json).collect::<Vec<_>>(),
            "members": chain.members().map(|m| m.id.clone()).collect::<Vec<_>>(),
            "removed": ["device-a"],
        },
        "invalidChains": [
            invalid("an edited entry", edited),
            invalid("a broken hash link", relinked),
            invalid("a skipped sequence number", resequenced),
            invalid("signed by a device outside the group", [&entries[..2], &[by_outsider]].concat()),
            invalid("a device adding itself after the first entry", [&entries[..2], &[self_add]].concat()),
            invalid("re-adding a removed device", [&entries[..], &[readd]].concat()),
            invalid("adding a key already in use", [&entries[..2], &[duplicate_key]].concat()),
            invalid("signed by a removed device", [&entries[..], &[signed_by_removed]].concat()),
            invalid("no first entry", entries[1..].to_vec()),
        ],
        "pairing": {
            "input": "oil0-23ab-cdef-ghjk-mnpq",
            "display": code.display(),
            "mailbox": code.mailbox(),
            "join": join,
            "welcome": welcome,
        },
        "noise": {
            "pattern": PATTERN,
            "client": "device-a",
            "host": host_id,
            "prologue": hex(&prologue(host_id)),
            "clientEphemeral": hex(&client_e),
            "hostEphemeral": hex(&host_e),
            "first": hex(&first),
            "reply": hex(&reply),
            "messages": messages,
        },
    })
}

#[test]
fn shared_vectors_match_the_committed_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../protocol/vectors/device-trust-v1.json"
    );
    let built = serde_json::to_string_pretty(&build()).unwrap() + "\n";
    if std::env::var_os("UPDATE_VECTORS").is_some() {
        std::fs::write(path, &built).unwrap();
    }
    let committed = std::fs::read_to_string(path).expect("run with UPDATE_VECTORS=1 to create it");
    assert!(
        committed == built,
        "{path} is stale; regenerate with UPDATE_VECTORS=1"
    );
}

#[test]
fn the_committed_chain_verifies_and_the_invalid_ones_do_not() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../protocol/vectors/device-trust-v1.json"
    );
    let file: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let entries: Vec<TrustEntry> =
        serde_json::from_value(file["chain"]["entries"].clone()).unwrap();
    let chain = TrustChain::verify(entries).unwrap();
    let members: Vec<_> = chain.members().map(|m| m.id.clone()).collect();
    assert_eq!(json!(members), file["chain"]["members"]);
    for case in file["invalidChains"].as_array().unwrap() {
        let entries: Vec<TrustEntry> = serde_json::from_value(case["entries"].clone()).unwrap();
        assert!(TrustChain::verify(entries).is_err(), "{}", case["name"]);
    }
}
