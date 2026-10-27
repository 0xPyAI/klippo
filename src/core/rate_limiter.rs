use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[derive(Clone)]
pub struct TokenBucketLimiter {
    state: std::sync::Arc<Mutex<LimiterState>>,
}

struct LimiterState {
    max_bytes_per_sec: u64,
    capacity: f64,
    tokens: f64,
    last_update: Instant,
}

fn capacity_for(max_bytes_per_sec: u64) -> f64 {
    if max_bytes_per_sec > 0 {
        (max_bytes_per_sec as f64).max(64.0 * 1024.0)
    } else {
        0.0
    }
}

impl TokenBucketLimiter {
    pub fn new(max_bytes_per_sec: u64) -> Self {
        let capacity = capacity_for(max_bytes_per_sec);
        Self {
            state: std::sync::Arc::new(Mutex::new(LimiterState {
                max_bytes_per_sec,
                capacity,
                tokens: capacity,
                last_update: Instant::now(),
            })),
        }
    }

    /// A limiter that never throttles (limit 0).
    pub fn unlimited() -> Self {
        Self::new(0)
    }

    pub fn limit(&self) -> u64 {
        self.state.lock().unwrap().max_bytes_per_sec
    }

    pub fn set_limit(&self, max_bytes_per_sec: u64) {
        let mut lock = self.state.lock().unwrap();
        lock.max_bytes_per_sec = max_bytes_per_sec;
        lock.capacity = capacity_for(max_bytes_per_sec);
        lock.tokens = lock.tokens.min(lock.capacity);
        lock.last_update = Instant::now();
    }

    pub async fn consume(&self, num_bytes: u64, mut cancel_rx: Option<&mut watch::Receiver<bool>>) {
        if num_bytes == 0 {
            return;
        }

        loop {
            if let Some(ref mut rx) = cancel_rx {
                if *rx.borrow() {
                    return;
                }
            }

            let sleep_duration = {
                let mut lock = self.state.lock().unwrap();
                if lock.max_bytes_per_sec == 0 {
                    return;
                }

                let now = Instant::now();
                let elapsed = now.duration_since(lock.last_update).as_secs_f64();
                lock.last_update = now;

                lock.tokens = (lock.tokens + elapsed * (lock.max_bytes_per_sec as f64)).min(lock.capacity);

                // A chunk bigger than the bucket can never fit, so it only waits for
                // a full bucket and then goes into debt; later chunks repay it.
                let needed = (num_bytes as f64).min(lock.capacity);
                if lock.tokens >= needed {
                    lock.tokens -= num_bytes as f64;
                    return;
                } else {
                    let deficit = needed - lock.tokens;
                    let secs = deficit / (lock.max_bytes_per_sec as f64);
                    Duration::from_secs_f64(secs.clamp(0.001, 0.05))
                }
            };

            tokio::time::sleep(sleep_duration).await;
        }
    }
}
