//! Plan bodies on the host (docs/design/plan-body.md §4, §8): the full text
//! of a plan whose `botWork` row had to cut it, in the workspace chat doc's
//! `planBodies` map.
//!
//! The host is the only writer. Every write goes doc first, then the row,
//! so no device sees a row pointing at a body the host failed to write; a
//! body whose row never landed (a crash in between, a failed row write) or
//! whose row was deleted elsewhere is an orphan, and the cleanup pass
//! ([`Bots::clean_plan_bodies`]) removes it once it is an hour old. The host
//! trusts only parts whose content hashes to their stored rev
//! ([`WorkBodies::intact`]).

use std::collections::BTreeMap;
use std::time::Duration;

use cypher_proto::{self as p, BotWork, PlanBody, PlanResult, PlanText, WorkBodies};

use crate::EngineError;
use crate::bots::Bots;

/// How long a body without a row is left alone: the window between the
/// doc write and the row write, with room for a slow registry.
pub(super) const ORPHAN_AGE_MS: i64 = 3_600_000;
/// How often the host's cleanup pass runs (it opens the chat docs).
pub(super) const CLEANUP_EVERY: Duration = Duration::from_secs(3_600);

/// Why an approved plan with a cut row didn't start: its body isn't on its
/// host, or isn't the one the row was written with. It never runs on the
/// previews.
pub(super) const MISSING_BODY: &str = "Its full text is missing on its host, so it didn't run on \
     the shortened preview. Propose the plan again.";

/// The `planBodies` keys the cleanup pass removes: no row of that id, and
/// written over [`ORPHAN_AGE_MS`] before `now_ms` (or unreadable, so with
/// no age at all).
pub(super) fn orphans(
    bodies: &BTreeMap<String, WorkBodies>,
    has_row: impl Fn(&str) -> bool,
    now_ms: i64,
) -> Vec<String> {
    bodies
        .iter()
        .filter(|(id, _)| !has_row(id))
        .filter(|(_, body)| {
            body.written_at()
                .is_none_or(|at| now_ms.saturating_sub(at) > ORPHAN_AGE_MS)
        })
        .map(|(id, _)| id.clone())
        .collect()
}

impl Bots {
    /// Write `planBodies[work_id].plan` into `chat_id`'s doc and release it
    /// to sync at once (the row follows).
    pub(super) fn write_plan_body(
        &self,
        chat_id: &str,
        work_id: &str,
        body: &PlanBody,
    ) -> Result<(), EngineError> {
        let handle = self.inner.doc_host.open(chat_id)?;
        handle.doc().put_plan_body(work_id, body)?;
        self.inner.doc_host.flush_chat_sync(chat_id);
        Ok(())
    }

    fn write_result_body(&self, work: &BotWork, result: &PlanResult) -> Result<(), EngineError> {
        let handle = self.inner.doc_host.open(&work.chat_id)?;
        handle.doc().put_plan_result(&work.id, result)?;
        self.inner.doc_host.flush_chat_sync(&work.chat_id);
        Ok(())
    }

    /// `work`'s bodies as the host trusts them (intact parts only); `None`
    /// without opening anything when the row names no body.
    pub(super) fn bodies_of(&self, work: &BotWork) -> Result<Option<WorkBodies>, EngineError> {
        if work.body_rev.is_none() && work.result_rev.is_none() {
            return Ok(None);
        }
        let handle = self.inner.doc_host.open(&work.chat_id)?;
        Ok(handle.doc().plan_body(&work.id).map(|body| body.intact()))
    }

    /// `work`'s text with its body: the full text where the body is here
    /// and matches, the row's preview where it isn't.
    pub(super) fn plan_text(&self, work: &BotWork) -> Result<PlanText, EngineError> {
        Ok(PlanText::of(work, self.bodies_of(work)?.as_ref()))
    }

    /// The `result` and `resultRev` for `work`'s row from the full `text`:
    /// when the row has to cut it, the body is written first. `row` is the
    /// row as it will be written (state and steps already ended), so the
    /// preview is measured against what the row holds. `Err` when the body
    /// had to be written and couldn't be, and nothing was written then.
    pub(super) fn result_fields(
        &self,
        mut row: BotWork,
        text: &str,
        now_ms: i64,
    ) -> Result<(Option<String>, Option<String>), EngineError> {
        let body = row
            .cut_result(text, now_ms)
            .map_err(|over| EngineError::Other(format!("The result is too long: {over}.")))?;
        if let Some(body) = &body {
            self.write_result_body(&row, body)?;
        }
        Ok((row.result, row.result_rev))
    }

    /// [`Self::result_fields`] for a result the host writes itself (a
    /// settled or stopped plan, quoting its thread): cut to
    /// [`p::RESULT_BODY_MAX`] with a mark, and when the doc write fails,
    /// the row keeps the preview alone (§4: the one cut left).
    pub(super) fn host_result_fields(
        &self,
        row: BotWork,
        text: &str,
        now_ms: i64,
    ) -> (Option<String>, Option<String>) {
        let text = p::clip_host_result(text);
        let work_id = row.id.clone();
        match self.result_fields(row, &text, now_ms) {
            Ok(fields) => fields,
            Err(err) => {
                tracing::warn!(
                    work = %work_id,
                    error = %err,
                    "the plan's full result couldn't be written to its chat; the row keeps a preview"
                );
                (Some(text).filter(|text| !text.is_empty()), None)
            }
        }
    }

    /// Remove the bodies of pruned work, with their rows. A row without a
    /// rev has nothing to remove (an orphan of it is the cleanup pass's).
    pub(super) fn delete_bodies(&self, work: &[&BotWork]) {
        for work in work
            .iter()
            .filter(|w| w.body_rev.is_some() || w.result_rev.is_some())
        {
            let deleted = self
                .inner
                .doc_host
                .open(&work.chat_id)
                .and_then(|handle| Ok(handle.doc().delete_plan_body(&work.id)?));
            if let Err(err) = deleted {
                tracing::warn!(work = %work.id, error = %err, "could not delete a pruned plan's body");
            }
        }
    }

    /// The cleanup pass (§8): in the chat of every bot hosted here, remove
    /// the `planBodies` entries whose work row is gone and that are over an
    /// hour old. The text leaves the doc's current state; its history keeps
    /// it (D6). Runs at engine start, then hourly; returns how many it
    /// removed. Public for tests, which pass a later `now_ms`.
    #[doc(hidden)]
    pub fn clean_plan_bodies(&self, now_ms: i64) -> usize {
        let ws = &self.inner.workspace;
        let bots = match ws.read_bots() {
            Ok(bots) => bots,
            Err(err) => {
                tracing::warn!(error = %err, "plan body cleanup: no bot rows");
                return 0;
            }
        };
        let mut removed = 0;
        for bot in bots
            .iter()
            .filter(|bot| bot.host_device_id == self.device_id())
        {
            let handle = match self.inner.doc_host.open(&bot.chat_id) {
                Ok(handle) => handle,
                Err(err) => {
                    tracing::warn!(bot = %bot.id, error = %err, "plan body cleanup: chat won't open");
                    continue;
                }
            };
            let bodies = handle.doc().plan_bodies();
            if bodies.is_empty() {
                continue;
            }
            // Any row of the id counts, even one this engine can't decode.
            let has_row = |id: &str| {
                ws.record::<serde_json::Value>(p::KIND_BOT_WORK, id)
                    .is_some()
            };
            for id in orphans(&bodies, has_row, now_ms) {
                match handle.doc().delete_plan_body(&id) {
                    Ok(true) => removed += 1,
                    Ok(false) => {}
                    Err(err) => {
                        tracing::warn!(work = %id, error = %err, "could not remove an orphaned plan body");
                    }
                }
            }
        }
        if removed > 0 {
            tracing::info!(removed, "removed orphaned plan bodies");
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(at: i64) -> WorkBodies {
        WorkBodies {
            plan: Some(PlanBody::new("i".into(), BTreeMap::new(), at)),
            result: None,
        }
    }

    #[test]
    fn orphans_are_bodies_without_rows_past_the_hour() {
        let now = 10 * ORPHAN_AGE_MS;
        let bodies: BTreeMap<String, WorkBodies> = [
            ("kept-row".to_string(), plan(0)),
            ("fresh".to_string(), plan(now - ORPHAN_AGE_MS)),
            ("old".to_string(), plan(now - ORPHAN_AGE_MS - 1)),
            ("torn".to_string(), WorkBodies::default()),
            (
                "result-old".to_string(),
                WorkBodies {
                    plan: Some(PlanBody::new("i".into(), BTreeMap::new(), 0)),
                    result: Some(PlanResult::new("r".into(), now)),
                },
            ),
        ]
        .into_iter()
        .collect();
        let found = orphans(&bodies, |id| id == "kept-row", now);
        // The newest part's age counts: a result written just now keeps it.
        assert_eq!(found, ["old", "torn"]);
    }
}
