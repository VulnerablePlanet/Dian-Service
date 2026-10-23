//! Casos de uso del motor de cumplimiento. Orquestan el dominio a través de sus puertos y no
//! conocen la base de datos, el transporte HTTP ni el cliente SOAP concretos.

pub mod admin;
pub mod inbound;
pub mod issue;
pub mod processing;
pub mod radian;
pub mod resilience;
pub mod webhooks;
pub mod xml;

#[cfg(test)]
pub(crate) mod test_support;
