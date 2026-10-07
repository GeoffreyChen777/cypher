//! Mock harness for engine/UI tests: replays a scripted event sequence.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_proto::{AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};

use crate::{Harness, HarnessError, RunControls};

pub struct MockHarness {
    pub script: Vec<AgentEvent>,
}

#[async_trait]
impl Harness for MockHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Mock"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![
            Model {
                id: "mock-1".into(),
                label: "Mock 1".into(),
                description: None,
                reasoning_levels: vec![ReasoningLevel::Medium],
                options: vec![],
            },
            // Claude-mirroring demo model: lets scripted runs carry the same
            // chip labels ("Fable 5 · High") as a real Claude session.
            Model {
                id: "mock-fable-5".into(),
                label: "Fable 5".into(),
                description: None,
                reasoning_levels: vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                    ReasoningLevel::XHigh,
                ],
                options: vec![],
            },
        ])
    }
    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        // Optional pacing knob for demos/manual testing: `CYPHER_MOCK_DELAY_MS`
        // spaces the scripted events out so live-run UI states (working
        // indicator, streaming fade, trailing tool-group auto-open) are
        // observable. Unset (the default, and in tests) streams instantly.
        let delay_ms = cypher_env::var("MOCK_DELAY_MS")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let delay = std::time::Duration::from_millis(delay_ms);

        let done_ix = self
            .script
            .iter()
            .position(|e| matches!(e, AgentEvent::Done { .. }))
            .unwrap_or(self.script.len());
        let (body, tail) = self.script.split_at(done_ix);
        // Dev/testing knob: `CYPHER_MOCK_THINK=1` opens the reply with scripted
        // thinking (`ReasoningDelta`, a few chunks so `CYPHER_MOCK_DELAY_MS`
        // shows it stream) — the data-side way to put the collapsed thought
        // toggle on screen with the mock harness.
        let mock_think = cypher_env::var("MOCK_THINK").is_some_and(|v| !v.is_empty() && v != "0");
        let think_events = mock_think
            .then(|| {
                [
                    "**Planning the answer**\n\n",
                    "The user wants the streaming path explained. ",
                    "Walk it in order — command, host, fold — ",
                    "then run the tests to confirm nothing regressed.",
                ]
                .map(|text| AgentEvent::ReasoningDelta { text: text.into() })
            })
            .into_iter()
            .flatten();
        // Thinking follows a leading SessionStarted, which resets the fold.
        let lead = body
            .iter()
            .take_while(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count();
        let events: Vec<Result<AgentEvent, HarnessError>> = body[..lead]
            .iter()
            .cloned()
            .chain(think_events)
            .chain(body[lead..].iter().cloned())
            .chain(tail.iter().cloned())
            .map(Ok)
            .collect();
        if delay_ms == 0 {
            return Ok(futures::stream::iter(events).boxed());
        }
        Ok(futures::stream::iter(events)
            .then(move |event| async move {
                tokio::time::sleep(delay).await;
                event
            })
            .boxed())
    }
}
