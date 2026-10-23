use crate::config::ApiConfig;
use application::inbound::InboundService;
use application::issue::IssueService;
use application::radian::RadianService;
use core_domain::ports::{DocumentRepository, HealthProbe, TenantRepository};
use std::sync::Arc;

/// Dependencias compartidas por los handlers. Solo contiene puertos y casos de uso: el
/// servidor HTTP no conoce la base de datos ni el cliente SOAP concretos.
pub struct AppState {
    pub tenants: Arc<dyn TenantRepository>,
    pub documents: Arc<dyn DocumentRepository>,
    pub health: Arc<dyn HealthProbe>,
    pub issue: IssueService,
    pub inbound: InboundService,
    pub radian: RadianService,
    pub config: ApiConfig,
    pub metrics: Arc<crate::metrics::HttpMetrics>,
}

pub type SharedState = Arc<AppState>;
