//! Núcleo del dominio: modelos, reglas de negocio puras y puertos. No depende de ninguna
//! tecnología de infraestructura (base de datos, colas, HTTP).

pub mod admin;
pub mod api_keys;
pub mod jobs;
pub mod lifecycle;
pub mod models;
pub mod money;
pub mod nit;
pub mod numbering;
pub mod ports;
#[cfg(feature = "testing")]
pub mod testing;
pub mod time;
pub mod webhooks;
