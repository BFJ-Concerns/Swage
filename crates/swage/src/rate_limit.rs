//! Rate limiting infrastructure for LLM provider requests.
//!
//! Provides transparent request rate limiting using a token bucket algorithm.
//! Tracks both request counts and token counts per minute to respect provider limits.
//!
//! # Design
//!
//! This rate limiter operates as a transparent delay mechanism:
//! - Requests are never rejected due to rate limits
//! - Instead, `acquire()` waits until sufficient capacity is available
//! - Separate tracking for request count and token count
//! - Thread-safe, async-compatible implementation
//!
//! # Provider-side 429 Handling
//!
//! This rate limiter handles *client-side* limiting only. Provider HTTP 429 responses
//! with retry-after headers are handled separately in the HTTP client layer.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Configuration for rate limiting.
///
/// Defines the maximum number of requests and tokens allowed per minute.
/// Both limits are enforced independently.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum number of requests allowed per minute.
    /// If `None`, no request count limit is enforced.
    pub requests_per_minute: Option<usize>,

    /// Maximum number of tokens allowed per minute.
    /// If `None`, no token count limit is enforced.
    pub tokens_per_minute: Option<usize>,
}

impl RateLimitConfig {
    /// Creates a new rate limit configuration.
    ///
    /// A capacity of 0 is treated as unlimited (None).
    /// This prevents division by zero in the token bucket refill calculations.
    pub fn new(requests_per_minute: Option<usize>, tokens_per_minute: Option<usize>) -> Self {
        Self {
            requests_per_minute: requests_per_minute.filter(|&n| n > 0),
            tokens_per_minute: tokens_per_minute.filter(|&n| n > 0),
        }
    }

    /// Creates a configuration with no limits.
    pub fn unlimited() -> Self {
        Self {
            requests_per_minute: None,
            tokens_per_minute: None,
        }
    }
}

/// Token bucket state for tracking usage within a time window.
#[derive(Debug)]
struct BucketState {
    /// Available tokens in the current window.
    available: f64,
    /// Maximum capacity of the bucket.
    capacity: f64,
    /// Rate at which tokens are replenished (tokens per second).
    refill_rate: f64,
    /// Last time the bucket was updated.
    last_update: Instant,
}

impl BucketState {
    /// Creates a new bucket with the given capacity per minute.
    fn new(capacity_per_minute: usize) -> Self {
        let capacity = capacity_per_minute as f64;
        Self {
            available: capacity,
            capacity,
            refill_rate: capacity / 60.0, // Convert per-minute to per-second
            last_update: Instant::now(),
        }
    }

    /// Refills the bucket based on elapsed time since last update.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();

        // Add tokens based on elapsed time
        self.available = (self.available + elapsed * self.refill_rate).min(self.capacity);
        self.last_update = now;
    }

    /// Calculates how long to wait before the requested tokens are available.
    ///
    /// Returns `None` if tokens are immediately available.
    fn time_until_available(&self, requested: f64) -> Option<Duration> {
        if self.available >= requested {
            return None;
        }

        let deficit = requested - self.available;
        let seconds = deficit / self.refill_rate;
        Some(Duration::from_secs_f64(seconds))
    }

    /// Consumes the requested number of tokens.
    ///
    /// Assumes sufficient tokens are available (caller must check first).
    fn consume(&mut self, amount: f64) {
        self.available -= amount;
    }
}

/// Thread-safe rate limiter using token bucket algorithm.
///
/// Tracks both request counts and token counts independently.
/// Waits transparently when limits are approached, never rejecting requests.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

#[derive(Debug)]
struct RateLimiterInner {
    request_bucket: Option<BucketState>,
    token_bucket: Option<BucketState>,
}

impl RateLimiter {
    /// Creates a new rate limiter with the given configuration.
    pub fn new(config: RateLimitConfig) -> Self {
        let request_bucket = config.requests_per_minute.map(BucketState::new);
        let token_bucket = config.tokens_per_minute.map(BucketState::new);

        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                request_bucket,
                token_bucket,
            })),
        }
    }

    /// Creates a rate limiter with no limits.
    pub fn unlimited() -> Self {
        Self::new(RateLimitConfig::unlimited())
    }

    /// Acquires permission to make a request consuming the specified number of tokens.
    ///
    /// This method waits (potentially multiple times) until both:
    /// - The request count limit allows another request
    /// - The token count limit allows consuming `token_count` tokens
    ///
    /// The method is transparent: it never rejects, only delays.
    ///
    /// # Arguments
    ///
    /// * `token_count` - Number of tokens this request will consume
    pub async fn acquire(&self, token_count: usize) {
        loop {
            let wait_duration = {
                let mut inner = self.inner.lock().await;

                // Refill both buckets based on elapsed time
                if let Some(bucket) = &mut inner.request_bucket {
                    bucket.refill();
                }
                if let Some(bucket) = &mut inner.token_bucket {
                    bucket.refill();
                }

                // Calculate how long we need to wait for both constraints
                let request_wait = inner
                    .request_bucket
                    .as_ref()
                    .and_then(|bucket| bucket.time_until_available(1.0));

                let token_wait = inner
                    .token_bucket
                    .as_ref()
                    .and_then(|bucket| bucket.time_until_available(token_count as f64));

                // Take the maximum of both wait times
                match (request_wait, token_wait) {
                    (None, None) => {
                        // Both constraints satisfied, consume and return
                        if let Some(bucket) = &mut inner.request_bucket {
                            bucket.consume(1.0);
                        }
                        if let Some(bucket) = &mut inner.token_bucket {
                            bucket.consume(token_count as f64);
                        }
                        None
                    }
                    (Some(d1), None) => Some(d1),
                    (None, Some(d2)) => Some(d2),
                    (Some(d1), Some(d2)) => Some(d1.max(d2)),
                }
            };

            match wait_duration {
                None => break, // Acquired successfully
                Some(duration) => {
                    // Wait before trying again
                    tokio::time::sleep(duration).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn config_unlimited_creates_no_limits() {
        let config = RateLimitConfig::unlimited();
        assert!(config.requests_per_minute.is_none());
        assert!(config.tokens_per_minute.is_none());
    }

    #[test]
    fn config_new_sets_limits() {
        let config = RateLimitConfig::new(Some(100), Some(50000));
        assert_eq!(config.requests_per_minute, Some(100));
        assert_eq!(config.tokens_per_minute, Some(50000));
    }

    #[test]
    fn config_treats_zero_as_unlimited() {
        // Zero capacity should be converted to None (unlimited)
        // to prevent division by zero in bucket refill calculations
        let config = RateLimitConfig::new(Some(0), Some(0));
        assert!(config.requests_per_minute.is_none());
        assert!(config.tokens_per_minute.is_none());
    }

    #[test]
    fn bucket_state_initialises_full() {
        let bucket = BucketState::new(60);
        assert_eq!(bucket.available, 60.0);
        assert_eq!(bucket.capacity, 60.0);
        assert_eq!(bucket.refill_rate, 1.0); // 60 per minute = 1 per second
    }

    #[test]
    fn bucket_state_refill_adds_tokens() {
        let mut bucket = BucketState::new(60);
        bucket.consume(30.0);
        assert_eq!(bucket.available, 30.0);

        // Manually adjust last_update to simulate passage of time
        bucket.last_update = Instant::now() - Duration::from_secs(10);
        bucket.refill();

        // Should have gained approximately 10 tokens (1 per second)
        assert!((bucket.available - 40.0).abs() < 0.1);
    }

    #[test]
    fn bucket_state_refill_caps_at_capacity() {
        let mut bucket = BucketState::new(60);
        bucket.last_update = Instant::now() - Duration::from_secs(120);
        bucket.refill();

        // Should cap at 60, not exceed it
        assert_eq!(bucket.available, 60.0);
    }

    #[test]
    fn bucket_state_time_until_available_immediate() {
        let bucket = BucketState::new(60);
        let wait = bucket.time_until_available(30.0);
        assert!(wait.is_none());
    }

    #[test]
    fn bucket_state_time_until_available_calculates_wait() {
        let mut bucket = BucketState::new(60);
        bucket.consume(50.0); // 10 remaining

        // Need 20 tokens, have 10, need 10 more at 1 per second
        let wait = bucket.time_until_available(20.0);
        assert!(wait.is_some());
        let duration = wait.unwrap();

        // Should be approximately 10 seconds
        assert!((duration.as_secs_f64() - 10.0).abs() < 0.1);
    }

    #[tokio::test]
    async fn rate_limiter_unlimited_allows_immediate_acquire() {
        let limiter = RateLimiter::unlimited();
        let start = Instant::now();

        // Should complete immediately with no delays
        limiter.acquire(1000).await;
        limiter.acquire(2000).await;
        limiter.acquire(3000).await;

        let elapsed = start.elapsed();
        // All three should complete in well under 100ms
        assert!(elapsed < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn rate_limiter_enforces_request_limit() {
        // Allow 2 requests per minute (2/60 = 0.0333... per second)
        let config = RateLimitConfig::new(Some(2), None);
        let limiter = RateLimiter::new(config);

        let start = Instant::now();

        // First two should be immediate
        limiter.acquire(0).await;
        limiter.acquire(0).await;

        // Third should wait approximately 30 seconds (for 1 token to refill)
        limiter.acquire(0).await;

        let elapsed = start.elapsed();
        // Should take at least 25 seconds (allowing some margin)
        assert!(elapsed >= Duration::from_secs(25));
    }

    #[tokio::test]
    async fn rate_limiter_enforces_token_limit() {
        // Allow 60 tokens per minute (1 per second)
        let config = RateLimitConfig::new(None, Some(60));
        let limiter = RateLimiter::new(config);

        let start = Instant::now();

        // Consume all 60 tokens
        limiter.acquire(60).await;

        // Next request needs to wait for refill
        limiter.acquire(10).await;

        let elapsed = start.elapsed();
        // Should wait at least 8 seconds for 10 tokens (allowing margin)
        assert!(elapsed >= Duration::from_secs(8));
    }

    #[tokio::test]
    async fn rate_limiter_enforces_both_limits() {
        // 3 requests per minute, 90 tokens per minute
        let config = RateLimitConfig::new(Some(3), Some(90));
        let limiter = RateLimiter::new(config);

        let start = Instant::now();

        // Use up request limit first (3 requests at 30 tokens each)
        limiter.acquire(30).await; // Request 1
        limiter.acquire(30).await; // Request 2
        limiter.acquire(30).await; // Request 3

        // Fourth request must wait for request bucket (20 seconds for 1 request)
        // Note: token bucket also empty but refills faster
        limiter.acquire(10).await;

        let elapsed = start.elapsed();
        // Should wait at least 15 seconds (allowing margin)
        assert!(elapsed >= Duration::from_secs(15));
    }

    #[tokio::test]
    async fn rate_limiter_is_thread_safe() {
        use tokio::task;

        let config = RateLimitConfig::new(Some(100), Some(5000));
        let limiter = RateLimiter::new(config);

        // Spawn multiple concurrent tasks
        let mut handles = vec![];
        for _ in 0..10 {
            let limiter_clone = limiter.clone();
            let handle = task::spawn(async move {
                limiter_clone.acquire(100).await;
            });
            handles.push(handle);
        }

        // All should complete without panicking
        for handle in handles {
            handle.await.unwrap();
        }
    }
}
