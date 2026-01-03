use std::time::Duration;
use tracing::info;

pub struct ReconnectStrategy {
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub multiplier: f64,
    attempts: u32,
}

impl ReconnectStrategy {
    pub fn new() -> Self {
        Self {
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            multiplier: 2.0,
            attempts: 0,
        }
    }

    pub fn reset(&mut self) {
        self.attempts = 0;
    }

    /// Get the current number of attempts
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub async fn wait(&mut self) {
        let delay = self.next_delay();
        info!("Reconnecting in {:?} (attempt {})", delay, self.attempts);
        tokio::time::sleep(delay).await;
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.base_delay.mul_f64(self.multiplier.powi(self.attempts as i32));
        self.attempts += 1;

        // Add some jitter (±10%)
        let jitter_factor = 0.9 + rand_f64() * 0.2;
        let delay_with_jitter = delay.mul_f64(jitter_factor);

        std::cmp::min(delay_with_jitter, self.max_delay)
    }
}

impl Default for ReconnectStrategy {
    fn default() -> Self {
        Self::new()
    }
}

// Simple random float between 0 and 1
fn rand_f64() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    (nanos % 1000) as f64 / 1000.0
}
