use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};

#[derive(Clone)]
pub struct SpeedLimiter {
    limit: Arc<AtomicU64>,
    tokens: Arc<tokio::sync::Mutex<TokenState>>,
}

struct TokenState {
    available: u64,
    last_refill: Instant,
}

impl SpeedLimiter {
    pub fn new(limit: u64) -> Self {
        Self {
            limit: Arc::new(AtomicU64::new(limit)),
            tokens: Arc::new(tokio::sync::Mutex::new(TokenState {
                available: limit,
                last_refill: Instant::now(),
            })),
        }
    }

    pub fn set_limit(&self, limit: u64) {
        self.limit.store(limit, Ordering::Relaxed);
    }

    pub async fn acquire(&self, bytes: u64) {
        let limit = self.limit.load(Ordering::Relaxed);
        if limit == 0 {
            return;
        }

        loop {
            let mut state = self.tokens.lock().await;
            let now = Instant::now();
            let elapsed = now.duration_since(state.last_refill);

            if elapsed >= Duration::from_secs(1) {
                state.available = limit;
                state.last_refill = now;
            } else {
                let refill = (elapsed.as_secs_f64() * limit as f64) as u64;
                if refill > 0 {
                    state.available = state.available.saturating_add(refill).min(limit);
                    state.last_refill = now;
                }
            }

            if state.available >= bytes {
                state.available -= bytes;
                return;
            }

            let deficit = bytes - state.available;
            let wait = Duration::from_secs_f64(deficit as f64 / limit as f64);
            drop(state);
            sleep(wait.min(Duration::from_millis(100))).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Instant;

    #[tokio::test]
    async fn test_acquire_no_limit() {
        // limit=0 意味着无限制，acquire 应立即返回
        let limiter = SpeedLimiter::new(0);
        let start = Instant::now();
        limiter.acquire(1_000_000).await;
        assert!(start.elapsed() < Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_acquire_within_budget() {
        // 请求量小于限制，不应阻塞
        let limiter = SpeedLimiter::new(1_000_000); // 1MB/s
        let start = Instant::now();
        limiter.acquire(100).await; // 100 bytes，远低于预算
        assert!(start.elapsed() < Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_acquire_exceeds_budget_blocks() {
        // 耗尽预算后，下一次 acquire 应阻塞
        let limiter = SpeedLimiter::new(1000); // 1000 bytes/s
        limiter.acquire(1000).await; // 耗尽全部预算
        let start = Instant::now();
        limiter.acquire(500).await; // 应阻塞约 500ms
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(50),
            "Should have blocked, elapsed: {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_set_limit_dynamic() {
        let limiter = SpeedLimiter::new(100);
        limiter.set_limit(0); // 禁用限制
        let start = Instant::now();
        limiter.acquire(999_999).await; // limit=0 应立即通过
        assert!(start.elapsed() < Duration::from_millis(10));
    }
}
