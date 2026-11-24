//! Plan bodies (docs/design/plan-body.md): a plan's long text, kept out of
//! its `botWork` row.
//!
//! The row stays the plan's index, with `instruction`, the steps' details
//! and `result` as previews made by [`BotWork::clamp`] at today's caps. When
//! that clamp had to cut something, the host keeps the full text in the
//! workspace chat doc's root map `planBodies[workId]` (`cypher_doc`'s
//! `SessionDoc::put_plan_body`) and stamps the row with its rev
//! (`bodyRev` / `resultRev`). A plan that fits is written exactly as before:
//! no body, no rev (decision D1).
//!
//! Readers decide with one rule, [`PlanText::of`]: a rev on the row means the
//! row's text is a preview; a body whose stored rev equals it is the full
//! text. Clients compare the two strings and never hash; only the host
//! computes revs ([`PlanBody::rev`]) and checks them ([`PlanBody::is_intact`]).
//!
//! Bodies have their own caps (§7): over them the model's text is refused
//! ([`OverCap`]), never cut. The two texts the model doesn't write are cut
//! instead and say so: the owner's instruction at [`INSTRUCTION_BODY_MAX`]
//! and a result the host quotes ([`clip_host_result`]).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::bot_records::clip;
use crate::{BotWork, WorkStep};

/// The steps' details together, in characters, in one plan's body. No step
/// has a cap of its own.
pub const PLAN_BODY_MAX: usize = 30_000;
/// A result, in characters: `finish_work` refuses one over it; a result the
/// host writes itself is cut to it ([`clip_host_result`]).
pub const RESULT_BODY_MAX: usize = 30_000;
/// The owner's instruction, in characters. It can't be refused, so it is cut
/// (an ellipsis marks it) and the model and the brief are told.
pub const INSTRUCTION_BODY_MAX: usize = 30_000;
/// A rev's length: the first 16 hex characters of a SHA-256.
pub const REV_LEN: usize = 16;
/// What a row keeps of a rev at most (a longer one is not the host's).
pub const REV_MAX: usize = 64;
/// How a result the host cut ends.
pub const HOST_CUT_MARK: &str = "…(cut)";

/// A text over its body cap: refused, with the overshoot for the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverCap {
    /// Its length, in characters.
    pub len: usize,
    /// The cap it is over.
    pub max: usize,
}

impl OverCap {
    /// How many characters too long it is.
    pub fn over(&self) -> usize {
        self.len.saturating_sub(self.max)
    }
}

impl fmt::Display for OverCap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} characters, {} over the {} it may hold",
            self.len,
            self.over(),
            self.max
        )
    }
}

impl std::error::Error for OverCap {}

fn count(text: &str) -> usize {
    text.trim().chars().count()
}

/// `Err` when `text` (trimmed) is over [`RESULT_BODY_MAX`]: what
/// `finish_work` refuses.
pub fn check_result_body(text: &str) -> Result<(), OverCap> {
    match count(text) {
        len if len > RESULT_BODY_MAX => Err(OverCap {
            len,
            max: RESULT_BODY_MAX,
        }),
        _ => Ok(()),
    }
}

/// A result the host writes itself (quoting the thread's last message, when
/// a plan settles or is stopped), trimmed to [`RESULT_BODY_MAX`]; a cut ends
/// in [`HOST_CUT_MARK`] so the reader knows.
pub fn clip_host_result(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= RESULT_BODY_MAX {
        return text.to_string();
    }
    let keep = RESULT_BODY_MAX - HOST_CUT_MARK.chars().count();
    let mut out: String = text.chars().take(keep).collect();
    out.truncate(out.trim_end().len());
    out.push_str(HOST_CUT_MARK);
    out
}

// ── revs ────────────────────────────────────────────────────────────────────

/// The rev of `value`: the first [`REV_LEN`] hex characters of SHA-256 over
/// its canonical JSON with any top-level `rev` left out. Canonical: no
/// whitespace, object keys sorted by their UTF-8 bytes at every level (so the
/// order serde_json keeps, which `preserve_order` makes insertion order,
/// doesn't matter).
pub fn body_rev<T: Serialize>(value: &T) -> String {
    let mut value = serde_json::to_value(value).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.remove("rev");
    }
    let mut json = String::new();
    canonical_json(&value, &mut json);
    let digest = Sha256::digest(json.as_bytes());
    let mut rev = String::with_capacity(REV_LEN);
    for byte in digest.iter().take(REV_LEN / 2) {
        rev.push_str(&format!("{byte:02x}"));
    }
    rev
}

fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical_json(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        leaf => out.push_str(&leaf.to_string()),
    }
}

// ── bodies ──────────────────────────────────────────────────────────────────

/// `planBodies[workId].plan`: the plan's full text, written once by its host
/// when it is proposed. A plain JSON value, written whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBody {
    /// [`Self::rev`] as the host computed it when writing; the row's
    /// `bodyRev` names it.
    #[serde(default)]
    pub rev: String,
    /// A copy of the owner's message (decision D3), at most
    /// [`INSTRUCTION_BODY_MAX`] characters.
    #[serde(default)]
    pub instruction: String,
    /// Every step's full detail by step id; steps without one are absent.
    #[serde(default)]
    pub details: BTreeMap<String, String>,
    /// Epoch millis it was written (the cleanup pass's clock).
    #[serde(default)]
    pub at: i64,
}

impl PlanBody {
    /// A body of these texts, its rev computed.
    pub fn new(instruction: String, details: BTreeMap<String, String>, at: i64) -> Self {
        let mut body = Self {
            rev: String::new(),
            instruction,
            details,
            at,
        };
        body.rev = body.rev();
        body
    }

    /// The rev of what it holds now ([`body_rev`]). Host-side only.
    pub fn rev(&self) -> String {
        body_rev(self)
    }

    /// Whether what it holds is what its stored rev was computed over: the
    /// host's check before it runs a plan (R5).
    pub fn is_intact(&self) -> bool {
        self.rev == self.rev()
    }

    /// Whether it is the body a row with `rev` was written with (a string
    /// comparison; clients never hash).
    pub fn matches(&self, rev: Option<&str>) -> bool {
        rev.is_some_and(|rev| !rev.is_empty() && rev == self.rev)
    }
}

/// `planBodies[workId].result`: the full result, written once by the host
/// when the plan ends, only when the row's result had to be cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanResult {
    /// The row's `resultRev` names it.
    #[serde(default)]
    pub rev: String,
    #[serde(default)]
    pub text: String,
    /// Epoch millis it was written.
    #[serde(default)]
    pub at: i64,
}

impl PlanResult {
    /// A result body of `text`, its rev computed.
    pub fn new(text: String, at: i64) -> Self {
        let mut result = Self {
            rev: String::new(),
            text,
            at,
        };
        result.rev = result.rev();
        result
    }

    /// The rev of what it holds now ([`body_rev`]). Host-side only.
    pub fn rev(&self) -> String {
        body_rev(self)
    }

    pub fn is_intact(&self) -> bool {
        self.rev == self.rev()
    }

    pub fn matches(&self, rev: Option<&str>) -> bool {
        rev.is_some_and(|rev| !rev.is_empty() && rev == self.rev)
    }
}

/// `planBodies[workId]`: what one work item keeps in the chat doc. Either
/// part may be missing (never cut, not written yet, or not synced yet).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkBodies {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<PlanResult>,
}

impl WorkBodies {
    /// Only the parts whose content hashes to their stored rev: what the
    /// host reads before it trusts a body (start, brief, `get_work`).
    pub fn intact(&self) -> Self {
        Self {
            plan: self.plan.clone().filter(PlanBody::is_intact),
            result: self.result.clone().filter(PlanResult::is_intact),
        }
    }

    /// When it was last written; `None` when neither part reads (a
    /// malformed entry the cleanup pass may remove at once).
    pub fn written_at(&self) -> Option<i64> {
        let plan = self.plan.as_ref().map(|plan| plan.at);
        let result = self.result.as_ref().map(|result| result.at);
        plan.max(result)
    }

    pub fn is_empty(&self) -> bool {
        self.plan.is_none() && self.result.is_none()
    }
}

// ── writing: the row's previews and the bodies they need ───────────────────

impl BotWork {
    /// `Err` when the steps' details together are over [`PLAN_BODY_MAX`]:
    /// what `propose_plan` refuses.
    pub fn check_plan_body(&self) -> Result<(), OverCap> {
        let len: usize = self
            .steps
            .iter()
            .filter_map(|step| step.detail.as_deref())
            .map(count)
            .sum();
        if len > PLAN_BODY_MAX {
            return Err(OverCap {
                len,
                max: PLAN_BODY_MAX,
            });
        }
        Ok(())
    }

    /// Turn a work item holding the plan's full text into its row: refuse
    /// details over [`PLAN_BODY_MAX`], cut the owner's instruction at
    /// [`INSTRUCTION_BODY_MAX`], then [`Self::clamp`] to previews. When the
    /// clamp cut the instruction or any detail, the body that holds the full
    /// text is returned and `bodyRev` set to its rev; when nothing was cut,
    /// `None` and the row is the whole plan (D1).
    ///
    /// Pure: the caller writes the body to the chat doc first, then the row.
    /// Call it once, on the full text (a row of previews has nothing left to
    /// keep).
    pub fn cut_plan(&mut self, at_ms: i64) -> Result<Option<PlanBody>, OverCap> {
        self.check_plan_body()?;
        self.instruction = clip(&self.instruction, INSTRUCTION_BODY_MAX);
        for step in &mut self.steps {
            step.detail = step
                .detail
                .as_deref()
                .map(str::trim)
                .filter(|detail| !detail.is_empty())
                .map(String::from);
        }
        let instruction = self.instruction.clone();
        let full_steps: Vec<WorkStep> = self.steps.clone();
        self.clamp();
        let cut = self.instruction != instruction
            || self.steps.len() != full_steps.len()
            || self
                .steps
                .iter()
                .zip(&full_steps)
                .any(|(row, full)| row.detail != full.detail);
        if !cut {
            return Ok(None);
        }
        let details = full_steps
            .into_iter()
            .filter_map(|step| Some((step.id, step.detail?)))
            .collect();
        let body = PlanBody::new(instruction, details, at_ms);
        self.body_rev = Some(body.rev.clone());
        // The rev takes a little room of its own.
        self.clamp();
        Ok(Some(body))
    }

    /// Set the result from its full `text`: refuse it over
    /// [`RESULT_BODY_MAX`] (a host-written result goes through
    /// [`clip_host_result`] first), then [`Self::clamp`]. When the row's
    /// result had to be cut, the body that holds it is returned and
    /// `resultRev` set; otherwise `None` and `resultRev` cleared.
    ///
    /// Pure, like [`Self::cut_plan`]: under a row lock, run it on a copy of
    /// the row first, write the body, then set `result` and `result_rev` from
    /// the copy inside the lock.
    pub fn cut_result(&mut self, text: &str, at_ms: i64) -> Result<Option<PlanResult>, OverCap> {
        check_result_body(text)?;
        let full = text.trim();
        self.result = Some(full.to_string()).filter(|text| !text.is_empty());
        self.result_rev = None;
        self.clamp();
        if full.is_empty() || self.result.as_deref() == Some(full) {
            return Ok(None);
        }
        let result = PlanResult::new(full.to_string(), at_ms);
        self.result_rev = Some(result.rev.clone());
        self.clamp();
        Ok(Some(result))
    }
}

// ── reading ─────────────────────────────────────────────────────────────────

/// Where a [`PlanText`] part comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSource {
    /// The row holds the whole text: no rev (nothing was cut, or the row is
    /// from a host that predates bodies).
    Row,
    /// The full text, from a body whose rev matches the row's.
    Body,
    /// The row's preview: the text was cut and its body isn't here yet (or
    /// is another rev). Readers mark it cut and show "Loading the full
    /// plan…" under it; Approve stays enabled (D5).
    Preview,
}

impl TextSource {
    pub fn is_preview(self) -> bool {
        self == Self::Preview
    }
}

/// A plan's text as a reader shows it: the full text when its body is
/// here, the row's preview otherwise. The one decision every reader makes
/// (R7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanText {
    pub instruction: String,
    /// Each step's detail, in the row's step order.
    pub details: Vec<Option<String>>,
    /// Where `instruction` and `details` come from.
    pub plan: TextSource,
    pub result: Option<String>,
    /// Where `result` comes from.
    pub result_source: TextSource,
}

impl PlanText {
    /// `work`'s text with `body` (its `planBodies` entry, when the reader has
    /// it). A body is used only when its stored rev equals the row's; a body
    /// with no rev on the row is ignored (the row is the index).
    pub fn of(work: &BotWork, body: Option<&WorkBodies>) -> Self {
        let plan_body = body
            .and_then(|body| body.plan.as_ref())
            .filter(|plan| plan.matches(work.body_rev.as_deref()));
        let result_body = body
            .and_then(|body| body.result.as_ref())
            .filter(|result| result.matches(work.result_rev.as_deref()));
        let row_details = work.steps.iter().map(|step| step.detail.clone());
        let (instruction, details, plan) = match (&work.body_rev, plan_body) {
            (None, _) => (
                work.instruction.clone(),
                row_details.collect(),
                TextSource::Row,
            ),
            (Some(_), Some(body)) => (
                body.instruction.clone(),
                work.steps
                    .iter()
                    .map(|step| {
                        body.details
                            .get(&step.id)
                            .cloned()
                            .or_else(|| step.detail.clone())
                    })
                    .collect(),
                TextSource::Body,
            ),
            (Some(_), None) => (
                work.instruction.clone(),
                row_details.collect(),
                TextSource::Preview,
            ),
        };
        let (result, result_source) = match (&work.result_rev, result_body) {
            (None, _) => (work.result.clone(), TextSource::Row),
            (Some(_), Some(body)) => (Some(body.text.clone()), TextSource::Body),
            (Some(_), None) => (work.result.clone(), TextSource::Preview),
        };
        Self {
            instruction,
            details,
            plan,
            result,
            result_source,
        }
    }

    /// Step `index`'s detail.
    pub fn detail(&self, index: usize) -> Option<&str> {
        self.details.get(index).and_then(|detail| detail.as_deref())
    }

    /// Whether any part shows a preview whose body is still to come.
    pub fn is_loading(&self) -> bool {
        self.plan.is_preview() || self.result_source.is_preview()
    }
}

#[cfg(test)]
mod tests;
