//! Safe, bounded runtime error reporting for Rust applications.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
mod analytics_wire;
#[cfg(any(feature = "tracing", feature = "log"))]
pub mod logging;
mod model;
mod owned_json;
pub mod privacy;
pub mod protocol;
#[cfg(feature = "local")]
pub mod relay;
#[cfg(feature = "testkit")]
pub mod testkit;
#[cfg(feature = "http")]
pub mod transport;
pub mod wire;
pub use model::{
    Context, DeploySnapshot, ExceptionInput, ExceptionSnapshot, ExceptionSource, Level,
    RequestSnapshot, TraceOrigin,
};
pub use model::{FrameClassification, FrameSnapshot};
mod config;
pub use config::{BeforeSend, Config, ConfigError, ProjectMode};
mod client;
pub mod delivery;
mod diagnostics;
mod identity;
mod recovery;
pub use recovery::{EvidenceScope, RecoveryObservation, RecoveryState};
pub fn new_evidence_id() -> Option<String> {
    identity::fresh()
}
pub fn init(config: Config) -> Client {
    Client::new(config)
}
mod occurrence;
mod operations;
pub use occurrence::ExceptionOccurrence;
pub use operations::{Operation, OperationContext, OperationKind, OperationOptions, ScopedFuture};
mod policy;
mod probes;
pub use probes::ProbeOptions;
#[cfg(any(feature = "tower", feature = "actix-web"))]
mod http_context;
mod runtime;
mod suppression;
#[cfg(any(feature = "tower", feature = "actix-web"))]
pub use http_context::{HttpOptions, RequestBodySnapshot};
#[cfg(feature = "actix-web")]
pub mod actix_web;
#[cfg(feature = "axum")]
pub mod axum;
#[cfg(feature = "tower")]
pub mod tower;
pub use client::{Client, ConnectionStatus, FlushOutcome, PanicObserver, Status};
#[cfg(feature = "local")]
pub mod storage;
