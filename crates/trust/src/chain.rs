//! The trust group: a hash-chained log of signed membership changes.
//!
//! Entry `0` is a device adding itself. Every later entry names the hash of
//! the one before it and is signed by a device that is a member at that point
//! in the log. A removed device stays removed (re-joining takes new keys), and
//! no key may appear in two entries' `add`s. Replaying the log from the start
//! yields the members; [`TrustChain::verify`] refuses any log that breaks a rule.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::TrustError;
use crate::encoding::{hex_bytes, put_str};
use crate::keys::{DeviceKeys, DevicePublic, MAX_FIELD};

/// The most entries a group may accumulate (adds plus removes).
pub const MAX_ENTRIES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustEntry {
    pub seq: u64,
    /// Hash of the previous entry; zeros for entry `0`.
    #[serde(with = "hex_bytes")]
    pub prev: [u8; 32],
    pub kind: EntryKind,
    /// The device added, or the removed member's record as the group knew it.
    pub device: DevicePublic,
    /// Milliseconds since the epoch, as claimed by the signer. Informational.
    pub at: u64,
    /// Id of the member that signed the entry.
    pub signer: String,
    #[serde(with = "hex_bytes")]
    pub sig: [u8; 64],
}

impl TrustEntry {
    /// The bytes the signer signs.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = b"cypher-trust-entry-v1\0".to_vec();
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.prev);
        out.push(match self.kind {
            EntryKind::Add => 1,
            EntryKind::Remove => 2,
        });
        self.device.put(&mut out);
        out.extend_from_slice(&self.at.to_be_bytes());
        put_str(&mut out, &self.signer);
        out
    }

    /// The hash the next entry names as `prev`.
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"cypher-trust-hash-v1\0");
        hasher.update(self.signing_bytes());
        hasher.update(self.sig);
        hasher.finalize().into()
    }

    fn signed(
        signer: &DeviceKeys,
        seq: u64,
        prev: [u8; 32],
        kind: EntryKind,
        device: DevicePublic,
        at: u64,
    ) -> Self {
        let mut entry = Self {
            seq,
            prev,
            kind,
            device,
            at,
            signer: signer.device_id.clone(),
            sig: [0; 64],
        };
        entry.sig = signer.sign(&entry.signing_bytes());
        entry
    }
}

/// How a log from the server relates to the one this device holds.
#[derive(Debug)]
pub enum Comparison {
    /// The same log.
    Same,
    /// The server's log extends ours: adopt it.
    Ahead(TrustChain),
    /// The server holds a strict prefix of ours (it lost entries, or never got
    /// them): publish the rest.
    Behind,
}

/// A verified log and the membership it yields.
#[derive(Debug, Clone, Default)]
pub struct TrustChain {
    entries: Vec<TrustEntry>,
    members: BTreeMap<String, DevicePublic>,
    removed: BTreeSet<String>,
    keys_seen: BTreeSet<[u8; 32]>,
}

impl TrustChain {
    /// Replay `entries` from the start, refusing any rule break.
    pub fn verify(entries: Vec<TrustEntry>) -> Result<Self, TrustError> {
        if entries.len() > MAX_ENTRIES {
            return Err(TrustError::InvalidEntry("too many entries".into()));
        }
        let mut chain = Self::default();
        for entry in entries {
            chain.apply(entry)?;
        }
        Ok(chain)
    }

    /// A new group whose only member is this device.
    pub fn genesis(keys: &DeviceKeys, name: &str, at: u64) -> Result<Self, TrustError> {
        let entry = TrustEntry::signed(keys, 0, [0; 32], EntryKind::Add, keys.public(name), at);
        Self::verify(vec![entry])
    }

    fn apply(&mut self, entry: TrustEntry) -> Result<(), TrustError> {
        let bad = |why: &str| Err(TrustError::InvalidEntry(why.into()));
        if entry.seq != self.entries.len() as u64 {
            return bad("out of sequence");
        }
        if entry.prev != self.head().unwrap_or([0; 32]) {
            return bad("does not follow the previous entry");
        }
        if entry.signer.is_empty() || entry.signer.len() > MAX_FIELD {
            return bad("bad signer");
        }
        entry.device.validate()?;
        let signer = if entry.seq == 0 {
            if entry.kind != EntryKind::Add || entry.signer != entry.device.id {
                return bad("the first entry must be a device adding itself");
            }
            &entry.device
        } else {
            match self.members.get(&entry.signer) {
                Some(signer) => signer,
                None => return bad("signed by a device outside the group"),
            }
        };
        if !signer.verify(&entry.signing_bytes(), &entry.sig) {
            return bad("bad signature");
        }
        match entry.kind {
            EntryKind::Add => {
                if self.members.contains_key(&entry.device.id)
                    || self.removed.contains(&entry.device.id)
                {
                    return bad("device already added or removed");
                }
                if self.keys_seen.contains(&entry.device.sign_pub)
                    || self.keys_seen.contains(&entry.device.dh_pub)
                {
                    return bad("key already used");
                }
                self.keys_seen.insert(entry.device.sign_pub);
                self.keys_seen.insert(entry.device.dh_pub);
                self.members
                    .insert(entry.device.id.clone(), entry.device.clone());
            }
            EntryKind::Remove => {
                if self.members.get(&entry.device.id) != Some(&entry.device) {
                    return bad("removes a device that is not a member");
                }
                self.members.remove(&entry.device.id);
                self.removed.insert(entry.device.id.clone());
            }
        }
        self.entries.push(entry);
        Ok(())
    }

    pub fn entries(&self) -> &[TrustEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn head(&self) -> Option<[u8; 32]> {
        self.entries.last().map(TrustEntry::hash)
    }

    pub fn members(&self) -> impl Iterator<Item = &DevicePublic> {
        self.members.values()
    }

    pub fn member(&self, device_id: &str) -> Option<&DevicePublic> {
        self.members.get(device_id)
    }

    pub fn member_by_dh(&self, dh_pub: &[u8; 32]) -> Option<&DevicePublic> {
        self.members
            .values()
            .find(|member| &member.dh_pub == dh_pub)
    }

    pub fn is_removed(&self, device_id: &str) -> bool {
        self.removed.contains(device_id)
    }

    /// The entry adding `device`, signed by member `signer`. Not yet applied.
    pub fn propose_add(
        &self,
        signer: &DeviceKeys,
        device: DevicePublic,
        at: u64,
    ) -> Result<TrustEntry, TrustError> {
        self.propose(signer, EntryKind::Add, device, at)
    }

    /// The entry removing member `device_id`, signed by member `signer`.
    pub fn propose_remove(
        &self,
        signer: &DeviceKeys,
        device_id: &str,
        at: u64,
    ) -> Result<TrustEntry, TrustError> {
        let device = self
            .members
            .get(device_id)
            .cloned()
            .ok_or_else(|| TrustError::InvalidEntry("not a member".into()))?;
        self.propose(signer, EntryKind::Remove, device, at)
    }

    fn propose(
        &self,
        signer: &DeviceKeys,
        kind: EntryKind,
        device: DevicePublic,
        at: u64,
    ) -> Result<TrustEntry, TrustError> {
        let entry = TrustEntry::signed(
            signer,
            self.entries.len() as u64,
            self.head().unwrap_or([0; 32]),
            kind,
            device,
            at,
        );
        // Dry run: the proposal must be valid on top of this log.
        self.clone().apply(entry.clone())?;
        Ok(entry)
    }

    /// Apply one more verified entry.
    pub fn append(&mut self, entry: TrustEntry) -> Result<(), TrustError> {
        if self.entries.len() >= MAX_ENTRIES {
            return Err(TrustError::InvalidEntry("too many entries".into()));
        }
        self.apply(entry)
    }

    /// Compare a log fetched from the server with this one. Never rolls back:
    /// a log that rewrites any entry this device holds is [`TrustError::Diverged`].
    pub fn compare(&self, remote: Vec<TrustEntry>) -> Result<Comparison, TrustError> {
        let shared = remote.len().min(self.entries.len());
        if remote[..shared] != self.entries[..shared] {
            return Err(TrustError::Diverged);
        }
        if remote.len() == self.entries.len() {
            return Ok(Comparison::Same);
        }
        if remote.len() < self.entries.len() {
            return Ok(Comparison::Behind);
        }
        Ok(Comparison::Ahead(Self::verify(remote)?))
    }

    /// The log up to and including `seq`, verified on its own.
    pub fn prefix(&self, seq: u64) -> Option<&[TrustEntry]> {
        self.entries
            .get(..usize::try_from(seq).ok()?.checked_add(1)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(id: &str) -> DeviceKeys {
        DeviceKeys::generate(id)
    }

    fn group() -> (DeviceKeys, DeviceKeys, TrustChain) {
        let a = keys("a");
        let b = keys("b");
        let mut chain = TrustChain::genesis(&a, "A", 1).unwrap();
        let add = chain.propose_add(&a, b.public("B"), 2).unwrap();
        chain.append(add).unwrap();
        (a, b, chain)
    }

    #[test]
    fn genesis_then_members_add_and_remove() {
        let (a, b, mut chain) = group();
        let c = keys("c");
        // Any member may add; the new member may then remove another.
        let add = chain.propose_add(&b, c.public("C"), 3).unwrap();
        chain.append(add).unwrap();
        let remove = chain.propose_remove(&c, "a", 4).unwrap();
        chain.append(remove).unwrap();
        let ids: Vec<_> = chain.members().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
        assert!(chain.is_removed("a"));
        // A removed device signs nothing more.
        assert!(chain.propose_add(&a, keys("d").public("D"), 5).is_err());
        // Its id cannot return.
        let again = keys("a");
        assert!(chain.propose_add(&b, again.public("A"), 5).is_err());
        // The whole log verifies from scratch.
        let replayed = TrustChain::verify(chain.entries().to_vec()).unwrap();
        assert_eq!(replayed.head(), chain.head());
    }

    #[test]
    fn outsiders_and_tampering_are_refused() {
        let (_, _, chain) = group();
        let outsider = keys("x");
        let entry = TrustEntry::signed(
            &outsider,
            2,
            chain.head().unwrap(),
            EntryKind::Add,
            outsider.public("X"),
            3,
        );
        assert!(
            chain.clone().append(entry).is_err(),
            "self-add after genesis"
        );

        let mut entries = chain.entries().to_vec();
        entries[1].device.name = "Renamed".into();
        assert!(TrustChain::verify(entries).is_err(), "edited entry");

        let mut entries = chain.entries().to_vec();
        entries[1].prev = [7; 32];
        assert!(TrustChain::verify(entries).is_err(), "broken link");

        let entries = chain.entries()[1..].to_vec();
        assert!(TrustChain::verify(entries).is_err(), "missing genesis");
    }

    #[test]
    fn a_key_cannot_be_added_twice() {
        let (a, b, chain) = group();
        let mut twin = keys("b2").public("B2");
        twin.dh_pub = b.public("B").dh_pub;
        assert!(chain.propose_add(&a, twin, 3).is_err());
    }

    #[test]
    fn the_server_log_only_ever_moves_forward() {
        let (a, _, chain) = group();
        let mut longer = chain.clone();
        longer
            .append(chain.propose_add(&a, keys("c").public("C"), 3).unwrap())
            .unwrap();

        assert!(matches!(
            chain.compare(chain.entries().to_vec()),
            Ok(Comparison::Same)
        ));
        assert!(matches!(
            chain.compare(longer.entries().to_vec()),
            Ok(Comparison::Ahead(next)) if next.head() == longer.head()
        ));
        assert!(matches!(
            longer.compare(chain.entries().to_vec()),
            Ok(Comparison::Behind)
        ));

        // A different second entry is a fork, whatever its length.
        let mut fork = TrustChain::genesis(&a, "A", 1).unwrap();
        fork.append(fork.propose_add(&a, keys("z").public("Z"), 2).unwrap())
            .unwrap();
        fork.append(fork.propose_add(&a, keys("y").public("Y"), 3).unwrap())
            .unwrap();
        assert_eq!(
            chain.compare(fork.entries().to_vec()).unwrap_err(),
            TrustError::Diverged
        );
    }
}
