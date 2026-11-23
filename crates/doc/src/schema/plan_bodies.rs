//! Plan bodies (docs/design/plan-body.md §3): the full text of a plan whose
//! `botWork` row had to cut it, in the workspace chat doc's root map
//!
//! ```text
//! planBodies: LoroMap {
//!   <workId>: LoroMap {
//!     plan:   { rev, instruction, details: { "<stepId>": "<full detail>" }, at }
//!     result: { rev, text, at }
//!   }
//! }
//! ```
//!
//! Only the bot's host writes it, and each part once ([`PlanBody`],
//! [`PlanResult`]): a plain JSON value written whole, so one write is atomic
//! and last-writer-wins. The row's `bodyRev` / `resultRev` point at a part;
//! readers resolve the two with `cypher_proto::PlanText::of`. Additive
//! container: docs that never held a long plan don't carry it, and peers
//! that predate it ignore it.

use std::collections::BTreeMap;

use cypher_proto::{PlanBody, PlanResult, WorkBodies};
use loro::{LoroMap, LoroValue, ToJson, ValueOrContainer};

use super::{SessionDoc, loro_value_from_json};
use crate::DocError;

/// The root map's name.
pub const PLAN_BODIES: &str = "planBodies";

impl SessionDoc {
    /// Write `planBodies[work_id].plan`, in one commit. Written once, when
    /// the plan is proposed; a second write replaces it whole.
    pub fn put_plan_body(&self, work_id: &str, body: &PlanBody) -> Result<(), DocError> {
        self.put_plan_part(work_id, "plan", serde_json::to_value(body)?)
    }

    /// Write `planBodies[work_id].result`, in one commit, beside the plan
    /// (which may be absent: a short plan can still end with a long result).
    pub fn put_plan_result(&self, work_id: &str, result: &PlanResult) -> Result<(), DocError> {
        self.put_plan_part(work_id, "result", serde_json::to_value(result)?)
    }

    fn put_plan_part(
        &self,
        work_id: &str,
        part: &str,
        value: serde_json::Value,
    ) -> Result<(), DocError> {
        // A plain child map (not loro's mergeable kind, which older peers'
        // loro may not read): only the host writes here, so no two devices
        // create the same entry.
        let map = self.doc.get_map(PLAN_BODIES);
        let entry = match map.get(work_id) {
            Some(ValueOrContainer::Container(loro::Container::Map(entry))) => entry,
            _ => map.insert_container(work_id, LoroMap::new())?,
        };
        entry.insert(part, loro_value_from_json(&value))?;
        self.doc.commit();
        Ok(())
    }

    /// Every entry of `planBodies`, by work id. A part that doesn't read (a
    /// newer or torn shape) is left out of its entry rather than failing the
    /// rest; an entry whose parts all fail still appears, empty, so the
    /// cleanup pass can see it.
    pub fn plan_bodies(&self) -> BTreeMap<String, WorkBodies> {
        let LoroValue::Map(entries) = self.doc.get_map(PLAN_BODIES).get_deep_value() else {
            return BTreeMap::new();
        };
        entries
            .iter()
            .map(|(work_id, entry)| (work_id.clone(), work_bodies(entry)))
            .collect()
    }

    /// `planBodies[work_id]`, when the doc has it.
    pub fn plan_body(&self, work_id: &str) -> Option<WorkBodies> {
        match self.doc.get_map(PLAN_BODIES).get(work_id)? {
            ValueOrContainer::Container(loro::Container::Map(entry)) => {
                Some(work_bodies(&entry.get_deep_value()))
            }
            ValueOrContainer::Value(value) => Some(work_bodies(&value)),
            ValueOrContainer::Container(_) => Some(WorkBodies::default()),
        }
    }

    /// Remove `planBodies[work_id]` from the doc's current state, in one
    /// commit (pruning, and the cleanup pass for orphans). Its text stays in
    /// the doc's history (D6). `false` when there was nothing to remove.
    pub fn delete_plan_body(&self, work_id: &str) -> Result<bool, DocError> {
        let map = self.doc.get_map(PLAN_BODIES);
        if map.get(work_id).is_none() {
            return Ok(false);
        }
        map.delete(work_id)?;
        self.doc.commit();
        Ok(true)
    }
}

fn work_bodies(entry: &LoroValue) -> WorkBodies {
    let LoroValue::Map(parts) = entry else {
        return WorkBodies::default();
    };
    let json = |key: &str| parts.get(key).map(LoroValue::to_json_value);
    WorkBodies {
        plan: json("plan").and_then(|value| serde_json::from_value(value).ok()),
        result: json("result").and_then(|value| serde_json::from_value(value).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loro::LoroDoc;

    fn body() -> PlanBody {
        let details = [
            ("1", "设".repeat(9_000)),
            ("2", "short \"quoted\"\n".into()),
        ]
        .into_iter()
        .map(|(id, detail)| (id.to_string(), detail))
        .collect();
        PlanBody::new("指".repeat(2_000), details, 1_700)
    }

    fn reopened(doc: &SessionDoc) -> SessionDoc {
        let other = LoroDoc::new();
        other.import(&doc.export_snapshot().unwrap()).unwrap();
        SessionDoc::from_doc(other)
    }

    #[test]
    fn bodies_round_trip_through_the_doc() {
        let doc = SessionDoc::init("chat-ws").unwrap();
        assert!(doc.plan_bodies().is_empty());
        assert_eq!(doc.plan_body("w1"), None);

        let plan = body();
        doc.put_plan_body("w1", &plan).unwrap();
        let result = PlanResult::new("果".repeat(5_000), 2_000);
        doc.put_plan_result("w1", &result).unwrap();
        // A short plan with a long result has a result only.
        let lone = PlanResult::new("r".repeat(3_000), 3_000);
        doc.put_plan_result("w2", &lone).unwrap();

        let read = reopened(&doc);
        let w1 = read.plan_body("w1").unwrap();
        assert_eq!(w1.plan.as_ref(), Some(&plan));
        assert_eq!(w1.result.as_ref(), Some(&result));
        assert!(
            w1.plan.as_ref().unwrap().is_intact(),
            "the rev survives the doc"
        );
        assert!(w1.result.as_ref().unwrap().is_intact());
        assert_eq!(w1.written_at(), Some(2_000));
        let all = read.plan_bodies();
        assert_eq!(all.keys().collect::<Vec<_>>(), ["w1", "w2"]);
        assert_eq!(all["w1"], w1);
        assert_eq!(
            all["w2"],
            WorkBodies {
                plan: None,
                result: Some(lone)
            }
        );

        // The result didn't touch the plan written before it.
        assert_eq!(doc.plan_body("w1").unwrap().plan, Some(plan));
    }

    #[test]
    fn the_shape_is_the_designed_one() {
        let doc = SessionDoc::init("chat-ws").unwrap();
        let plan = PlanBody::new("i".into(), [("1".into(), "d".into())].into(), 5);
        doc.put_plan_body("w1", &plan).unwrap();
        let json = doc
            .doc
            .get_map(PLAN_BODIES)
            .get_deep_value()
            .to_json_value();
        assert_eq!(
            json,
            serde_json::json!({ "w1": { "plan": {
                "rev": plan.rev, "instruction": "i", "details": { "1": "d" }, "at": 5,
            } } })
        );
        // Plain values, not containers: written whole, in one go.
        let entry = match doc.doc.get_map(PLAN_BODIES).get("w1") {
            Some(ValueOrContainer::Container(loro::Container::Map(entry))) => entry,
            other => panic!("entry is a map container, got {other:?}"),
        };
        assert!(matches!(
            entry.get("plan"),
            Some(ValueOrContainer::Value(_))
        ));
    }

    #[test]
    fn deleting_removes_the_entry_and_syncs() {
        let doc = SessionDoc::init("chat-ws").unwrap();
        doc.put_plan_body("w1", &body()).unwrap();
        doc.put_plan_result("w1", &PlanResult::new("r".into(), 1))
            .unwrap();
        doc.put_plan_body("w2", &body()).unwrap();
        let peer = reopened(&doc);

        assert!(doc.delete_plan_body("w1").unwrap());
        assert!(!doc.delete_plan_body("w1").unwrap(), "already gone");
        assert!(!doc.delete_plan_body("never").unwrap());
        assert_eq!(doc.plan_body("w1"), None);
        assert_eq!(doc.plan_bodies().keys().collect::<Vec<_>>(), ["w2"]);

        // A device that had the body loses it once the delete syncs.
        assert!(peer.plan_body("w1").is_some());
        peer.doc
            .import(&doc.doc.export(loro::ExportMode::all_updates()).unwrap())
            .unwrap();
        assert_eq!(peer.plan_body("w1"), None);
        assert!(peer.plan_body("w2").is_some());
    }

    #[test]
    fn malformed_parts_read_as_missing() {
        let doc = SessionDoc::init("chat-ws").unwrap();
        let map = doc.doc.get_map(PLAN_BODIES);
        let bad = map.insert_container("bad", LoroMap::new()).unwrap();
        bad.insert("plan", "not a body").unwrap();
        bad.insert(
            "result",
            loro_value_from_json(&serde_json::json!({ "rev": "r", "text": "t", "at": 1 })),
        )
        .unwrap();
        map.insert("flat", 7).unwrap();
        doc.doc.commit();

        let bad = doc.plan_body("bad").unwrap();
        assert_eq!(bad.plan, None);
        assert_eq!(bad.result.as_ref().map(|r| r.text.as_str()), Some("t"));
        assert!(
            !bad.result.unwrap().is_intact(),
            "its rev isn't its content's"
        );
        let all = doc.plan_bodies();
        assert!(all["flat"].is_empty(), "listed, so cleanup can remove it");
        assert_eq!(doc.plan_body("flat"), Some(WorkBodies::default()));
        assert!(doc.delete_plan_body("flat").unwrap());
    }
}
