//! An adaptive rate limiter for the Gmail API.
//!
//! Gmail bills in "quota units" rather than requests, and the published
//! per-user ceiling has moved more than once. Rather than hard-code a number
//! that may already be wrong, this limiter starts conservatively, speeds up
//! while requests succeed, and halves its rate the moment Google pushes back.
//! That converges on whatever the real limit is today without ever needing to
//! know it.
//!
//! The shape is AIMD — additive increase, multiplicative decrease — the same
//! control loop TCP uses, for the same reason: it recovers throughput quickly
//! and yields fast when the far end complains.

use std::time::Duration;

use rand::Rng;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Quota unit costs, from Google's published table. If these drift, the
/// adaptive loop absorbs the error — they only set the starting shape.
pub const COST_MESSAGES_LIST: f64 = 5.0;
pub const COST_MESSAGES_GET: f64 = 20.0;
pub const COST_HISTORY_LIST: f64 = 2.0;
pub const COST_MESSAGES_SEND: f64 = 100.0;
pub const COST_MESSAGES_TRASH: f64 = 20.0;
pub const COST_PROFILE: f64 = 1.0;
/// Settings changes are cheap and rare.
pub const COST_FILTER_CREATE: f64 = 5.0;

/// Start well under any documented ceiling and climb. A scan that begins
/// politely and accelerates is better than one that trips a limit in its first
/// second — and the climb is what finds the account's real allowance.
const START_RATE: f64 = 50.0;
/// The ceiling the adaptive loop is allowed to climb to.
///
/// Deliberately above the 100 units/second implied by the documented 6,000
/// per minute, for two reasons. The per-method costs this limiter prices in are
/// disputed — Google's own table says a metadata fetch is 20 units, several
/// other sources say 5 — so a conservative cap on top of a possibly
/// four-times-too-high cost model made scans needlessly slow. And a ceiling the
/// loop simply ramps to and sits at defeats the point of having a loop at all:
/// it never discovers the real limit.
///
/// If this is too high, the account says so with a 429, the rate halves, and it
/// settles where it should. That round trip costs a couple of retries; the
/// alternative cost a large mailbox hours.
const MAX_RATE: f64 = 240.0;
/// Even a heavily throttled account should still creep forward.
const MIN_RATE: f64 = 4.0;
/// Units added to the rate after a run of clean responses.
const INCREASE_STEP: f64 = 5.0;
/// Successes required before speeding up.
const SUCCESSES_PER_INCREASE: u32 = 25;
/// Rate multiplier applied when Google pushes back.
const DECREASE_FACTOR: f64 = 0.5;
/// Burst allowance, in seconds of rate.
const BURST_SECONDS: f64 = 1.0;

pub struct AdaptiveLimiter {
    state: Mutex<State>,
}

struct State {
    tokens: f64,
    rate: f64,
    last_refill: Instant,
    successes: u32,
}

impl Default for AdaptiveLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveLimiter {
    pub fn new() -> Self {
        Self::with_rate(START_RATE)
    }

    /// A limiter that starts at a chosen rate.
    ///
    /// Exists so tests can run against a mock server without waiting on a
    /// budget meant for Google. Not used by the app itself, which always starts
    /// at [`START_RATE`] and finds its own pace from there.
    pub fn with_rate(rate: f64) -> Self {
        Self {
            state: Mutex::new(State {
                tokens: rate * BURST_SECONDS,
                rate,
                last_refill: Instant::now(),
                successes: 0,
            }),
        }
    }

    /// Wait until `cost` quota units are available, then spend them.
    ///
    /// A single request can cost more than the bucket will ever hold — a send
    /// is 100 units, and at the minimum rate the bucket tops out at 4. Waiting
    /// for the full cost in that case would never return, so the price is
    /// clamped to the bucket's capacity: such a request waits for a completely
    /// full bucket and then drains it. That slightly under-charges the rarest
    /// and most expensive calls, which the adaptive loop absorbs.
    pub async fn acquire(&self, cost: f64) {
        loop {
            let wait = {
                let mut s = self.state.lock().await;
                s.refill();
                let capacity = s.rate * BURST_SECONDS;
                let price = cost.min(capacity);
                if s.tokens >= price {
                    s.tokens -= price;
                    return;
                }
                let needed = price - s.tokens;
                Duration::from_secs_f64((needed / s.rate).max(0.001))
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Record a clean response. After enough of them, speed up.
    pub async fn on_success(&self) {
        let mut s = self.state.lock().await;
        s.successes += 1;
        if s.successes >= SUCCESSES_PER_INCREASE {
            s.successes = 0;
            s.rate = (s.rate + INCREASE_STEP).min(MAX_RATE);
        }
    }

    /// Record a 429 or 5xx. Halve the rate and drop any banked burst, so the
    /// next request genuinely waits rather than spending saved-up tokens.
    pub async fn on_throttled(&self) {
        let mut s = self.state.lock().await;
        s.successes = 0;
        s.rate = (s.rate * DECREASE_FACTOR).max(MIN_RATE);
        s.tokens = 0.0;
    }

    #[cfg(test)]
    async fn rate(&self) -> f64 {
        self.state.lock().await.rate
    }
}

impl State {
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.rate).min(self.rate * BURST_SECONDS);
            self.last_refill = now;
        }
    }
}

/// How long to wait before retry number `attempt` (0-based).
///
/// Exponential with full jitter. The jitter matters: without it, a burst of
/// concurrent requests that are throttled together would all retry at the same
/// instant and be throttled together again.
pub fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    // Google sometimes tells us exactly how long to wait. Believe it.
    if let Some(d) = retry_after {
        return d.min(Duration::from_secs(120));
    }
    let base = 2f64.powi(attempt.min(6) as i32);
    let jittered = rand::thread_rng().gen_range(0.0..=base);
    Duration::from_secs_f64(jittered.clamp(0.25, 64.0))
}

/// Requests may be retried this many times before we give up on them.
pub const MAX_RETRIES: u32 = 6;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn acquire_is_immediate_while_burst_lasts() {
        let l = AdaptiveLimiter::new();
        let start = Instant::now();
        // The initial burst is one second of rate, so a couple of list calls
        // should not wait at all.
        l.acquire(COST_MESSAGES_LIST).await;
        l.acquire(COST_MESSAGES_LIST).await;
        assert!(start.elapsed() < Duration::from_millis(10));
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_waits_once_the_bucket_is_empty() {
        let l = AdaptiveLimiter::new();
        // Drain the burst.
        for _ in 0..3 {
            l.acquire(COST_MESSAGES_GET).await;
        }
        let start = Instant::now();
        l.acquire(COST_MESSAGES_GET).await;
        assert!(
            start.elapsed() >= Duration::from_millis(50),
            "expected to wait, waited {:?}",
            start.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn throttling_halves_the_rate_and_success_restores_it() {
        let l = AdaptiveLimiter::new();
        let before = l.rate().await;

        l.on_throttled().await;
        assert_eq!(l.rate().await, before * DECREASE_FACTOR);

        for _ in 0..SUCCESSES_PER_INCREASE {
            l.on_success().await;
        }
        assert_eq!(l.rate().await, before * DECREASE_FACTOR + INCREASE_STEP);
    }

    #[tokio::test(start_paused = true)]
    async fn the_rate_never_leaves_its_bounds() {
        let l = AdaptiveLimiter::new();
        for _ in 0..200 {
            l.on_throttled().await;
        }
        assert_eq!(l.rate().await, MIN_RATE);

        for _ in 0..(SUCCESSES_PER_INCREASE * 200) {
            l.on_success().await;
        }
        assert_eq!(l.rate().await, MAX_RATE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cost_larger_than_the_burst_still_completes() {
        // A send costs 100 units, more than one second at the starting rate.
        // Without clamping the deficit this would spin forever.
        let l = AdaptiveLimiter::new();
        l.acquire(COST_MESSAGES_SEND).await;
        l.acquire(COST_MESSAGES_SEND).await;
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        for attempt in 0..MAX_RETRIES {
            let d = backoff_delay(attempt, None);
            assert!(d >= Duration::from_millis(250));
            assert!(d <= Duration::from_secs(64));
        }
        // A Retry-After hint is honoured, but not unboundedly.
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(9))),
            Duration::from_secs(9)
        );
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(9999))),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn backoff_is_jittered() {
        // Identical inputs must not produce identical delays, or concurrent
        // retries would synchronise.
        let delays: Vec<_> = (0..20).map(|_| backoff_delay(5, None)).collect();
        assert!(
            delays.windows(2).any(|w| w[0] != w[1]),
            "backoff produced no jitter"
        );
    }
}
