//! Live output throughput for the working trailer's tok/s.
//!
//! Pi reports an assistant message's output tokens only when the message ends,
//! so the live rate is an ESTIMATE made from the streamed deltas themselves —
//! text, thinking, and tool-call arguments, which are all output — and each
//! finished message's reported count recalibrates the estimate for the next.
//! Run state only: the engine mirrors readings onto the local session
//! projection and nothing persists them.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use cypher_proto::Throughput;

/// The rate is averaged over this much recent streaming.
const WINDOW: Duration = Duration::from_secs(3);
/// No rate until a message has streamed this long: the first deltas of a
/// message arrive in a burst and read as a meaningless spike.
const WARMUP: Duration = Duration::from_millis(500);
/// Minimum gap between published readings while a message streams — the
/// trailer redraws on each, and deltas arrive far faster than it can matter.
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// Calibration needs a message long enough for the ratio to mean anything.
const CALIBRATION_MIN_RAW: f64 = 64.0;
/// A provider whose reported count strays this far from the character
/// estimate is counting something the stream never showed (hidden
/// reasoning); clamping keeps one such message from wrecking the next.
const CALIBRATION_RANGE: (f64, f64) = (0.5, 2.0);

/// Uncalibrated token estimate for streamed text. Latin text runs about four
/// characters to a token; CJK and other non-ASCII scripts about one and a
/// half. Calibration absorbs the model-specific remainder.
fn raw_tokens(text: &str) -> f64 {
    text.chars()
        .map(|c| if c.is_ascii() { 0.25 } else { 0.7 })
        .sum()
}

pub(crate) struct ThroughputMeter {
    /// Calibrated tokens per delta within [`WINDOW`], oldest first.
    samples: VecDeque<(Instant, f64)>,
    /// When the current message's first delta arrived.
    streaming_since: Option<Instant>,
    /// Uncalibrated estimate of the current message so far.
    message_raw: f64,
    /// Whether the current message streamed any thinking.
    message_thinking: bool,
    /// Output tokens of this turn's finished messages.
    turn_tokens: u64,
    /// Reported ÷ estimated, from the last message that reported. Kept
    /// across turns: the model rarely changes between them.
    calibration: f64,
    last_published: Option<Instant>,
}

impl Default for ThroughputMeter {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            streaming_since: None,
            message_raw: 0.0,
            message_thinking: false,
            turn_tokens: 0,
            calibration: 1.0,
            last_published: None,
        }
    }
}

impl ThroughputMeter {
    /// A new turn: its token count starts from zero.
    pub(crate) fn start_turn(&mut self) {
        self.samples.clear();
        self.streaming_since = None;
        self.message_raw = 0.0;
        self.message_thinking = false;
        self.turn_tokens = 0;
        self.last_published = None;
    }

    /// A new assistant message opens (a tool round-trip ended the last one).
    pub(crate) fn start_message(&mut self) {
        self.samples.clear();
        self.streaming_since = None;
        self.message_raw = 0.0;
        self.message_thinking = false;
    }

    /// One streamed delta. Returns a reading when one is due.
    pub(crate) fn delta(&mut self, text: &str, thinking: bool, now: Instant) -> Option<Throughput> {
        if text.is_empty() {
            return None;
        }
        let raw = raw_tokens(text);
        self.message_raw += raw;
        self.message_thinking |= thinking;
        self.streaming_since.get_or_insert(now);
        self.samples.push_back((now, raw * self.calibration));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            self.samples.pop_front();
        }
        let due = self
            .last_published
            .is_none_or(|last| now.duration_since(last) >= PUBLISH_EVERY);
        if !due {
            return None;
        }
        self.last_published = Some(now);
        Some(self.reading(now))
    }

    /// The message ended with its reported usage (`output`, and `reasoning`
    /// when the provider breaks it out). Always returns a reading, without a
    /// rate: nothing streams until the next message, so the trailer keeps the
    /// count and drops the speed while a tool runs.
    pub(crate) fn end_message(
        &mut self,
        output: Option<u64>,
        reasoning: Option<u64>,
    ) -> Throughput {
        let estimate = (self.message_raw * self.calibration).round() as u64;
        match output.filter(|&n| n > 0) {
            Some(output) => {
                self.turn_tokens += output;
                // Reasoning the provider never streamed (it only reports the
                // count) is no part of what the deltas measured.
                let streamed = if self.message_thinking {
                    output
                } else {
                    output.saturating_sub(reasoning.unwrap_or(0))
                };
                if self.message_raw >= CALIBRATION_MIN_RAW && streamed > 0 {
                    let (low, high) = CALIBRATION_RANGE;
                    self.calibration = (streamed as f64 / self.message_raw).clamp(low, high);
                }
            }
            None => self.turn_tokens += estimate,
        }
        self.start_message();
        Throughput {
            tokens_per_second: None,
            output_tokens: self.turn_tokens,
            sampled_at: chrono::Utc::now(),
        }
    }

    fn reading(&self, now: Instant) -> Throughput {
        let tokens_per_second = self.streaming_since.and_then(|since| {
            let streamed = now.duration_since(since);
            if streamed < WARMUP {
                return None;
            }
            let span = streamed.min(WINDOW).as_secs_f64();
            let tokens: f64 = self.samples.iter().map(|(_, n)| n).sum();
            Some((tokens / span).round() as u32)
        });
        Throughput {
            tokens_per_second,
            output_tokens: self.turn_tokens + (self.message_raw * self.calibration).round() as u64,
            sampled_at: chrono::Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// 40 ASCII chars = 10 uncalibrated tokens.
    fn chunk() -> String {
        "x".repeat(40)
    }

    #[test]
    fn rate_waits_out_the_warmup_then_tracks_the_window() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        let first = meter
            .delta(&chunk(), false, t0)
            .expect("first delta publishes");
        assert_eq!(first.tokens_per_second, None, "no rate inside the warmup");
        assert_eq!(first.output_tokens, 10);
        // 10 tokens every 100ms = 100 tok/s.
        let mut last = None;
        for step in 1..=20 {
            if let Some(reading) = meter.delta(&chunk(), false, t0 + 100 * step * MS) {
                last = Some(reading);
            }
        }
        let reading = last.expect("readings keep coming");
        let rate = reading.tokens_per_second.expect("a rate after warmup");
        assert!((95..=115).contains(&rate), "rate {rate}");
        assert_eq!(reading.output_tokens, 210);
    }

    #[test]
    fn readings_are_throttled() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        assert!(meter.delta("a", false, t0).is_some());
        assert!(meter.delta("b", false, t0 + 100 * MS).is_none());
        assert!(meter.delta("c", false, t0 + 500 * MS).is_some());
        assert!(
            meter.delta("", false, t0 + 2000 * MS).is_none(),
            "empty deltas are ignored"
        );
    }

    #[test]
    fn a_reported_count_replaces_the_estimate_and_calibrates_the_next_message() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        for step in 0..10 {
            meter.delta(&chunk(), false, t0 + 100 * step * MS);
        }
        // Estimated 100; the provider says 150.
        let ended = meter.end_message(Some(150), None);
        assert_eq!(ended.output_tokens, 150);
        assert_eq!(ended.tokens_per_second, None, "no rate between messages");
        meter.start_message();
        let next = meter.delta(&chunk(), false, t0 + 5000 * MS).unwrap();
        assert_eq!(next.output_tokens, 150 + 15, "10 raw tokens now count 1.5×");
    }

    #[test]
    fn unstreamed_reasoning_counts_toward_the_turn_but_not_the_calibration() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        for step in 0..10 {
            meter.delta(&chunk(), false, t0 + 100 * step * MS);
        }
        // 900 hidden reasoning tokens: the 100 visible ones matched exactly.
        assert_eq!(meter.end_message(Some(1000), Some(900)).output_tokens, 1000);
        let next = meter.delta(&chunk(), false, t0 + 5000 * MS).unwrap();
        assert_eq!(next.output_tokens, 1010, "calibration stays 1×");
    }

    #[test]
    fn calibration_is_clamped_and_needs_a_real_sample() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        // Too short to calibrate: the estimate stands in for nothing.
        meter.delta("hi", false, t0);
        assert_eq!(meter.end_message(Some(40), None).output_tokens, 40);
        meter.delta(&chunk(), false, t0 + 1000 * MS);
        assert_eq!(
            meter.end_message(None, None).output_tokens,
            50,
            "unreported → estimate"
        );
        // Wildly off: clamped to 2×.
        for step in 0..10 {
            meter.delta(&chunk(), false, t0 + (2000 + 100 * step) * MS);
        }
        meter.end_message(Some(10_000), None);
        let next = meter.delta(&chunk(), false, t0 + 9000 * MS).unwrap();
        assert_eq!(next.output_tokens, 10_050 + 20);
    }

    #[test]
    fn a_new_turn_starts_counting_from_zero() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        meter.delta(&chunk(), false, t0);
        meter.end_message(Some(10), None);
        meter.start_turn();
        let reading = meter.delta(&chunk(), false, t0 + 1000 * MS).unwrap();
        assert_eq!(reading.output_tokens, 10);
    }

    #[test]
    fn cjk_counts_denser_than_latin() {
        assert_eq!(raw_tokens("abcd"), 1.0);
        assert!(raw_tokens("你好世界") > 2.5);
    }
}
