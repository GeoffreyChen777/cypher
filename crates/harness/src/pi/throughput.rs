//! Live output throughput for the working trailer's tok/s.
//!
//! Pi reports an assistant message's output tokens only when the message ends,
//! so the live rate is an ESTIMATE made from the streamed deltas themselves —
//! text, thinking, and tool-call arguments, which are all output — and each
//! finished message's reported count recalibrates the estimate for the next.
//! A finished message also yields its average speed, which the trailer falls
//! back to whenever no live rate is due: between messages, on other devices,
//! and for providers that hold a message back and deliver it in one burst
//! (Claude Code after a tool result — no live rate can measure that).
//! Run state only: the engine mirrors readings onto the session projection
//! and nothing persists them.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use cypher_proto::Throughput;

/// The rate is averaged over this much recent streaming.
const WINDOW: Duration = Duration::from_secs(3);
/// No rate until a streak has run this long: the first deltas after a pause
/// arrive in a burst and read as a meaningless spike — or, when only the
/// burst's first delta is published, as a meaningless 0–1 tok/s.
const WARMUP: Duration = Duration::from_millis(500);
/// A pause this long between deltas ends a streak. A real stream's deltas
/// come tens of milliseconds apart; a longer silence is the provider working
/// unseen, and averaging across it measures nothing.
const STREAK_GAP: Duration = Duration::from_secs(1);
/// Minimum gap between published readings while a message streams — the
/// trailer redraws on each, and deltas arrive far faster than it can matter.
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// Calibration needs a message long enough for the ratio to mean anything.
const CALIBRATION_MIN_RAW: f64 = 64.0;
/// A provider whose reported count strays this far from the character
/// estimate is counting something the stream never showed (hidden
/// reasoning); clamping keeps one such message from wrecking the next.
const CALIBRATION_RANGE: (f64, f64) = (0.5, 2.0);
/// A message average needs this many tokens: a few tokens behind a slow
/// first response measure latency, not speed.
const AVERAGE_MIN_TOKENS: u64 = 16;

/// Uncalibrated token estimate for streamed text. Latin text runs about four
/// characters to a token; CJK and other non-ASCII scripts about one and a
/// half. Calibration absorbs the model-specific remainder.
fn raw_tokens(text: &str) -> f64 {
    text.chars()
        .map(|c| if c.is_ascii() { 0.25 } else { 0.7 })
        .sum()
}

pub struct ThroughputMeter {
    /// Calibrated tokens per delta of the current streak within [`WINDOW`],
    /// oldest first.
    samples: VecDeque<(Instant, f64)>,
    /// When the current streak's first delta arrived.
    streak_since: Option<Instant>,
    /// When the current message opened, and its latest delta arrived: the
    /// span its average speed is measured over.
    message_started: Option<Instant>,
    last_delta: Option<Instant>,
    /// Uncalibrated estimate of the current message so far.
    message_raw: f64,
    /// Whether the current message streamed any thinking.
    message_thinking: bool,
    /// Output tokens of this turn's finished messages.
    turn_tokens: u64,
    /// Average speed of this turn's last finished message that had one.
    average: Option<u32>,
    /// Reported ÷ estimated, from the last message that reported. Kept
    /// across turns: the model rarely changes between them.
    calibration: f64,
    last_published: Option<Instant>,
}

impl Default for ThroughputMeter {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            streak_since: None,
            message_started: None,
            last_delta: None,
            message_raw: 0.0,
            message_thinking: false,
            turn_tokens: 0,
            average: None,
            calibration: 1.0,
            last_published: None,
        }
    }
}

impl ThroughputMeter {
    /// A new turn: its token count and speed start from nothing.
    pub fn start_turn(&mut self) {
        self.reset_message(None);
        self.turn_tokens = 0;
        self.average = None;
        self.last_published = None;
    }

    /// A new assistant message opens (a tool round-trip ended the last one).
    pub fn start_message(&mut self, now: Instant) {
        self.reset_message(Some(now));
    }

    fn reset_message(&mut self, started: Option<Instant>) {
        self.samples.clear();
        self.streak_since = None;
        self.message_started = started;
        self.last_delta = None;
        self.message_raw = 0.0;
        self.message_thinking = false;
    }

    /// One streamed delta. Returns a reading when one is due.
    pub fn delta(&mut self, text: &str, thinking: bool, now: Instant) -> Option<Throughput> {
        if text.is_empty() {
            return None;
        }
        let raw = raw_tokens(text);
        self.message_raw += raw;
        self.message_thinking |= thinking;
        self.message_started.get_or_insert(now);
        if self
            .last_delta
            .is_none_or(|last| now.duration_since(last) > STREAK_GAP)
        {
            self.samples.clear();
            self.streak_since = Some(now);
        }
        self.last_delta = Some(now);
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
    /// live rate: nothing streams until the next message, so the trailer
    /// keeps the count and the message's average speed while a tool runs.
    pub fn end_message(&mut self, output: Option<u64>, reasoning: Option<u64>) -> Throughput {
        let estimate = (self.message_raw * self.calibration).round() as u64;
        let message_tokens = match output.filter(|&n| n > 0) {
            Some(output) => {
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
                output
            }
            None => estimate,
        };
        self.turn_tokens += message_tokens;
        // Measured to the last delta, not to now: a provider may close the
        // message seconds after its content arrived. Too short a span (the
        // whole message landed at once) keeps the previous average.
        if let (Some(started), Some(last)) = (self.message_started, self.last_delta) {
            let span = last.duration_since(started);
            if span >= WARMUP && message_tokens >= AVERAGE_MIN_TOKENS {
                self.average = Some((message_tokens as f64 / span.as_secs_f64()).round() as u32);
            }
        }
        self.reset_message(None);
        Throughput {
            tokens_per_second: None,
            average_tokens_per_second: self.average,
            output_tokens: self.turn_tokens,
            sampled_at: chrono::Utc::now(),
        }
    }

    fn reading(&self, now: Instant) -> Throughput {
        let tokens_per_second = self.streak_since.and_then(|since| {
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
            average_tokens_per_second: self.average,
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
        meter.start_message(t0 + 5000 * MS);
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
    fn a_burst_after_silence_reads_no_rate() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        meter.start_message(t0);
        meter.delta("One", false, t0);
        // Five silent seconds, then the rest of the message at once; only
        // the burst's first delta is due for publishing.
        let burst = t0 + 5000 * MS;
        let reading = meter.delta(&chunk(), false, burst).expect("due");
        assert_eq!(reading.tokens_per_second, None, "{reading:?}");
        for step in 1..20 {
            assert!(
                meter
                    .delta(&chunk(), false, burst + step * 2 * MS)
                    .is_none()
            );
        }
    }

    #[test]
    fn a_pause_starts_a_new_streak() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        meter.start_message(t0);
        for step in 0..10 {
            meter.delta(&chunk(), false, t0 + 100 * step * MS);
        }
        // 1.5s of silence: the next delta opens a streak that has to warm up.
        let resumed = t0 + 2400 * MS;
        let reading = meter.delta(&chunk(), false, resumed).expect("due");
        assert_eq!(reading.tokens_per_second, None);
        let mut last = None;
        for step in 1..=10 {
            if let Some(reading) = meter.delta(&chunk(), false, resumed + 100 * step * MS) {
                last = Some(reading);
            }
        }
        let rate = last.and_then(|r| r.tokens_per_second).expect("warmed up");
        assert!((95..=115).contains(&rate), "rate {rate}");
    }

    #[test]
    fn a_finished_message_reports_its_average_until_the_turn_ends() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        meter.start_message(t0);
        // Two unseen seconds, then deltas up to 4s; the provider closes the
        // message a further 5s on, which the average must not count.
        meter.delta(&chunk(), true, t0 + 2000 * MS);
        meter.delta(&chunk(), false, t0 + 4000 * MS);
        let ended = meter.end_message(Some(400), None);
        assert_eq!(ended.tokens_per_second, None);
        assert_eq!(ended.average_tokens_per_second, Some(100));
        // The next message carries it until it has an average of its own.
        meter.start_message(t0 + 10_000 * MS);
        let next = meter.delta(&chunk(), false, t0 + 10_000 * MS).unwrap();
        assert_eq!(next.average_tokens_per_second, Some(100));
        meter.start_turn();
        let fresh = meter.delta(&chunk(), false, t0 + 20_000 * MS).unwrap();
        assert_eq!(fresh.average_tokens_per_second, None);
    }

    #[test]
    fn a_message_that_lands_at_once_keeps_the_previous_average() {
        let mut meter = ThroughputMeter::default();
        let t0 = Instant::now();
        meter.start_message(t0);
        meter.delta(&chunk(), false, t0);
        meter.delta(&chunk(), false, t0 + 1000 * MS);
        assert_eq!(
            meter.end_message(Some(50), None).average_tokens_per_second,
            Some(50)
        );
        let start = t0 + 3000 * MS;
        meter.start_message(start);
        for step in 0..10 {
            meter.delta(&chunk(), false, start + step * 3 * MS);
        }
        let ended = meter.end_message(Some(600), None);
        assert_eq!(
            ended.average_tokens_per_second,
            Some(50),
            "a 27ms span measures nothing"
        );
        assert_eq!(ended.output_tokens, 650);
    }

    #[test]
    fn cjk_counts_denser_than_latin() {
        assert_eq!(raw_tokens("abcd"), 1.0);
        assert!(raw_tokens("你好世界") > 2.5);
    }
}
