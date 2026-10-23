//! Circuit breaker por ambiente DIAN y backoff con jitter.

use rand::Rng;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open(Instant),
    /// Se dejó pasar una única petición de prueba en `Instant`.
    HalfOpen(Instant),
}

/// Circuit breaker de un ambiente. En estado medio abierto deja pasar una sola petición de
/// prueba; si esa petición no reporta resultado dentro del enfriamiento, permite otra.
pub struct CircuitBreaker {
    state: Mutex<(BreakerState, u32)>,
    failure_threshold: u32,
    cooldown: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: Mutex::new((BreakerState::Closed, 0)),
            failure_threshold,
            cooldown,
        }
    }

    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    pub async fn can_execute(&self) -> bool {
        let mut guard = self.state.lock().await;
        match guard.0 {
            BreakerState::Closed => true,
            BreakerState::Open(opened_at) | BreakerState::HalfOpen(opened_at) => {
                if opened_at.elapsed() >= self.cooldown {
                    guard.0 = BreakerState::HalfOpen(Instant::now());
                    true
                } else {
                    false
                }
            }
        }
    }

    pub async fn record_success(&self) {
        *self.state.lock().await = (BreakerState::Closed, 0);
    }

    pub async fn record_failure(&self) {
        let mut guard = self.state.lock().await;
        guard.1 += 1;
        if matches!(guard.0, BreakerState::HalfOpen(_)) || guard.1 >= self.failure_threshold {
            guard.0 = BreakerState::Open(Instant::now());
        }
    }

    pub async fn state(&self) -> BreakerState {
        self.state.lock().await.0
    }
}

/// Un circuit breaker por ambiente: una caída de habilitación no bloquea producción.
pub struct Breakers {
    pub habilitacion: CircuitBreaker,
    pub produccion: CircuitBreaker,
}

impl Breakers {
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            habilitacion: CircuitBreaker::new(failure_threshold, cooldown),
            produccion: CircuitBreaker::new(failure_threshold, cooldown),
        }
    }

    pub fn for_environment(&self, is_hab: bool) -> &CircuitBreaker {
        if is_hab {
            &self.habilitacion
        } else {
            &self.produccion
        }
    }
}

/// Espera exponencial con jitter (entre 500 ms y `min(base * 2^attempt, max)`).
pub fn backoff_with_jitter(attempt: i32, base: Duration, max: Duration) -> Duration {
    let exponent = attempt.clamp(0, 16) as u32;
    let ceiling = base
        .saturating_mul(2u32.saturating_pow(exponent))
        .min(max)
        .max(Duration::from_millis(500));
    let millis = rand::thread_rng().gen_range(500..=ceiling.as_millis() as u64);
    Duration::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_after_threshold_and_allows_a_single_probe() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(50));
        cb.record_failure().await;
        assert!(cb.can_execute().await);
        cb.record_failure().await;
        assert!(!cb.can_execute().await);

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(cb.can_execute().await, "first probe after cooldown");
        assert!(!cb.can_execute().await, "only one probe while half-open");

        cb.record_failure().await;
        assert!(matches!(cb.state().await, BreakerState::Open(_)), "failed probe reopens");

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(cb.can_execute().await);
        cb.record_success().await;
        assert_eq!(cb.state().await, BreakerState::Closed);
        assert!(cb.can_execute().await && cb.can_execute().await);
    }

    #[tokio::test]
    async fn environments_are_isolated() {
        let breakers = Breakers::new(1, Duration::from_secs(60));
        breakers.for_environment(true).record_failure().await;
        assert!(!breakers.for_environment(true).can_execute().await);
        assert!(breakers.for_environment(false).can_execute().await);
    }

    #[test]
    fn backoff_stays_within_bounds() {
        for attempt in 0..20 {
            let d = backoff_with_jitter(attempt, Duration::from_secs(1), Duration::from_secs(60));
            assert!(d >= Duration::from_millis(500) && d <= Duration::from_secs(60));
        }
    }
}
