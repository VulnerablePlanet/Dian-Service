use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Open(Instant),
    HalfOpen,
}

pub struct CircuitBreaker {
    state: Arc<Mutex<State>>,
    consecutive_failures: Arc<Mutex<u32>>,
    failure_threshold: u32,
    cooldown: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::Closed)),
            consecutive_failures: Arc::new(Mutex::new(0)),
            failure_threshold,
            cooldown,
        }
    }

    /// Determina si el circuit breaker permite realizar la petición.
    /// Si el estado es Open pero pasó el tiempo de cooldown, transita automáticamente a HalfOpen.
    pub async fn can_execute(&self) -> bool {
        let mut state = self.state.lock().await;
        match *state {
            State::Closed => true,
            State::Open(opened_at) => {
                if opened_at.elapsed() >= self.cooldown {
                    *state = State::HalfOpen;
                    true
                } else {
                    false
                }
            }
            State::HalfOpen => true,
        }
    }

    /// Registra una llamada exitosa a la DIAN. Resetea los fallos y cierra el circuito.
    pub async fn record_success(&self) {
        let mut state = self.state.lock().await;
        let mut failures = self.consecutive_failures.lock().await;
        *failures = 0;
        *state = State::Closed;
    }

    /// Registra un fallo de comunicación. Si se excede el límite de fallos consecutivos, abre el circuito.
    pub async fn record_failure(&self) {
        let mut state = self.state.lock().await;
        let mut failures = self.consecutive_failures.lock().await;
        *failures += 1;

        if *failures >= self.failure_threshold {
            *state = State::Open(Instant::now());
        }
    }

    /// Retorna el estado actual para monitoreo/observabilidad.
    pub async fn get_state(&self) -> State {
        *self.state.lock().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_circuit_breaker_flow() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(100));

        // Inicialmente cerrado
        assert!(cb.can_execute().await);

        // Primer fallo
        cb.record_failure().await;
        assert!(cb.can_execute().await);

        // Segundo fallo -> Se abre el circuito
        cb.record_failure().await;
        assert!(!cb.can_execute().await);
        assert!(matches!(cb.get_state().await, State::Open(_)));

        // Esperar cooldown de 100ms
        tokio::time::sleep(Duration::from_millis(110)).await;

        // can_execute() debe transitar a HalfOpen
        assert!(cb.can_execute().await);
        assert_eq!(cb.get_state().await, State::HalfOpen);

        // Registrar éxito -> Cierra el circuito
        cb.record_success().await;
        assert!(cb.can_execute().await);
        assert_eq!(cb.get_state().await, State::Closed);
    }
}
