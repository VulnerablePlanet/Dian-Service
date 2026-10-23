//! Métricas del worker: resultado y duración de cada trabajo, trabajos en curso, profundidad
//! de la cola y estado de los circuit breakers de la DIAN.

use application::resilience::{BreakerState, Breakers};
use core_domain::jobs::QueueDepth;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct JobLabels {
    pub kind: String,
    /// `handled` (procesado, aunque haya programado un reintento), `error` o `timeout`.
    pub outcome: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct KindLabel {
    pub kind: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StateLabel {
    pub state: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct EnvironmentLabel {
    pub environment: String,
}

#[derive(Clone)]
pub struct WorkerMetrics {
    jobs: Family<JobLabels, Counter>,
    duration: Family<KindLabel, Histogram>,
    pub in_flight: Gauge,
    queue: Family<StateLabel, Gauge>,
    breaker_open: Family<EnvironmentLabel, Gauge>,
}

impl Default for WorkerMetrics {
    fn default() -> Self {
        Self {
            jobs: Family::default(),
            // 50 ms .. ~100 s
            duration: Family::new_with_constructor(|| Histogram::new(exponential_buckets(0.05, 2.0, 12))),
            in_flight: Gauge::default(),
            queue: Family::default(),
            breaker_open: Family::default(),
        }
    }
}

impl WorkerMetrics {
    pub fn register(&self, registry: &mut Registry) {
        registry.register("jobs", "Jobs processed by kind and outcome", self.jobs.clone());
        registry.register("job_duration_seconds", "Job processing time", self.duration.clone());
        registry.register("jobs_in_flight", "Jobs being processed by this worker", self.in_flight.clone());
        registry.register("queue_jobs", "Pending jobs by state (ready, scheduled, running, dead)", self.queue.clone());
        registry.register(
            "dian_breaker_open",
            "1 while the DIAN circuit breaker of the environment is open or half-open",
            self.breaker_open.clone(),
        );
    }

    pub fn job_finished(&self, kind: &str, outcome: &str, seconds: f64) {
        self.jobs
            .get_or_create(&JobLabels { kind: kind.to_string(), outcome: outcome.to_string() })
            .inc();
        self.duration.get_or_create(&KindLabel { kind: kind.to_string() }).observe(seconds);
    }

    pub fn set_queue_depth(&self, depth: QueueDepth) {
        for (state, value) in [
            ("ready", depth.ready),
            ("scheduled", depth.scheduled),
            ("running", depth.running),
            ("dead", depth.dead),
        ] {
            self.queue.get_or_create(&StateLabel { state: state.to_string() }).set(value);
        }
    }

    pub async fn sample_breakers(&self, breakers: &Breakers) {
        for (environment, breaker) in [("habilitacion", &breakers.habilitacion), ("produccion", &breakers.produccion)] {
            let open = !matches!(breaker.state().await, BreakerState::Closed);
            self.breaker_open
                .get_or_create(&EnvironmentLabel { environment: environment.to_string() })
                .set(i64::from(open));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus_client::encoding::text::encode;
    use std::time::Duration;

    #[tokio::test]
    async fn exposes_job_queue_and_breaker_metrics() {
        let metrics = WorkerMetrics::default();
        let mut registry = Registry::with_prefix("dian_worker");
        metrics.register(&mut registry);

        metrics.job_finished("transmit", "handled", 0.2);
        metrics.job_finished("transmit", "timeout", 540.0);
        metrics.set_queue_depth(QueueDepth { ready: 4, scheduled: 2, running: 1, dead: 3 });
        let breakers = Breakers::new(1, Duration::from_secs(60));
        breakers.produccion.record_failure().await;
        metrics.sample_breakers(&breakers).await;

        let mut text = String::new();
        encode(&mut text, &registry).unwrap();
        for expected in [
            r#"dian_worker_jobs_total{kind="transmit",outcome="handled"} 1"#,
            r#"dian_worker_jobs_total{kind="transmit",outcome="timeout"} 1"#,
            r#"dian_worker_queue_jobs{state="dead"} 3"#,
            r#"dian_worker_queue_jobs{state="ready"} 4"#,
            r#"dian_worker_dian_breaker_open{environment="produccion"} 1"#,
            r#"dian_worker_dian_breaker_open{environment="habilitacion"} 0"#,
        ] {
            assert!(text.contains(expected), "missing {}\n{}", expected, text);
        }
    }
}
