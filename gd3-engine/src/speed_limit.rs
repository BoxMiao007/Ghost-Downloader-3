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
