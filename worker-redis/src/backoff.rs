use rand::Rng;
use std::time::Duration;

/// Calcula el tiempo de espera con Jitter completo basándose en el intento.
/// Esto evita el efecto de estampida en los servidores de la DIAN.
pub fn calculate_backoff_with_jitter(attempt: u32) -> Duration {
    let base_ms = 1000; // Base de 1 segundo
    let max_ms = 60000; // Máximo de 60 segundos (1 minuto)

    // factor = 2^attempt
    let factor = 2u64.pow(attempt.min(6)); // Cap a 2^6 = 64
    let backoff_ms = (base_ms * factor).min(max_ms);

    // Full Jitter: rand(0, backoff_ms)
    let mut rng = rand::thread_rng();
    let jittered_ms = rng.gen_range(500..=backoff_ms); // Al menos 500ms

    Duration::from_millis(jittered_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_limits() {
        let backoff_1 = calculate_backoff_with_jitter(1);
        let backoff_10 = calculate_backoff_with_jitter(10);

        assert!(backoff_1.as_millis() <= 2000);
        assert!(backoff_10.as_millis() <= 60000);
        assert!(backoff_1.as_millis() >= 500);
        assert!(backoff_10.as_millis() >= 500);
    }
}
