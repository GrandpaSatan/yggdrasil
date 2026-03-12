use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::debug;

use crate::CloudProvider;

/// Token bucket rate limiter for cloud API calls.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<TokenBucket>>,
    provider: CloudProvider,
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter.
    /// - `requests_per_minute`: maximum requests per minute
    /// - `provider`: the cloud provider this limiter is for
    pub fn new(requests_per_minute: u32, provider: CloudProvider) -> Self {
        let max_tokens = requests_per_minute as f64;
        let refill_rate = max_tokens / 60.0;

        Self {
            inner: Arc::new(Mutex::new(TokenBucket {
                tokens: max_tokens,
                max_tokens,
                refill_rate,
                last_refill: Instant::now(),
            })),
            provider,
        }
    }

    /// Wait until a token is available, then consume it.
    pub async fn acquire(&self) {
        loop {
            let wait_time = {
                let mut bucket = self.inner.lock().await;
                bucket.refill();

                if bucket.tokens >= 1.0 {
                    bucket.tokens -= 1.0;
                    debug!(
                        provider = %self.provider,
                        remaining = bucket.tokens as u32,
                        "rate limit token acquired"
                    );
                    return;
                }

                // Calculate how long to wait for 1 token
                Duration::from_secs_f64(1.0 / bucket.refill_rate)
            };

            debug!(
                provider = %self.provider,
                wait_ms = wait_time.as_millis() as u64,
                "rate limited — waiting for token"
            );
            tokio::time::sleep(wait_time).await;
        }
    }

    /// Check if a token is available without consuming it.
    pub async fn available(&self) -> bool {
        let mut bucket = self.inner.lock().await;
        bucket.refill();
        bucket.tokens >= 1.0
    }
}

impl TokenBucket {
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new(60, CloudProvider::Openai);

        // Should succeed immediately — bucket starts full at 60 tokens
        limiter.acquire().await;
        assert!(limiter.available().await);
    }

    #[tokio::test]
    async fn test_rate_limiter_refills_over_time() {
        // Create a limiter with 60 RPM (1 token/sec refill rate)
        let limiter = RateLimiter::new(60, CloudProvider::Gemini);

        // Drain all tokens
        for _ in 0..60 {
            limiter.acquire().await;
        }

        // Bucket should be empty now
        assert!(!limiter.available().await);

        // Wait enough time for at least 1 token to refill (1 token/sec)
        tokio::time::sleep(Duration::from_millis(1100)).await;

        // Should have at least 1 token now
        assert!(limiter.available().await);
    }
}
