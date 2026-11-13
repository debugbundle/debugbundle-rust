//! Explicit application knowledge; no retry or incident lifecycle inference.
use serde_json::{json, Value};
use std::time::Duration;
#[derive(Clone, Debug)]
pub enum EvidenceScope {
    Operation { operation_id: String },
    Page { page_id: String },
}
impl EvidenceScope {
    pub(crate) fn value(&self) -> Option<Value> {
        let value = match self {
            Self::Operation { operation_id } => {
                json!({"kind":"operation","operation_id":operation_id})
            }
            Self::Page { page_id } => json!({"kind":"page","page_id":page_id}),
        };
        crate::wire::valid_reference(&json!({"version":1,"failure_ref":"00000000-0000-4000-8000-000000000000","scope":value})).then_some(value)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub enum RecoveryState {
    #[default]
    AttemptStarted,
    AttemptCompleted,
    AttemptFailed,
    RecoveryObserved,
    FailurePersisted,
}
impl RecoveryState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AttemptStarted => "attempt_started",
            Self::AttemptCompleted => "attempt_completed",
            Self::AttemptFailed => "attempt_failed",
            Self::RecoveryObserved => "recovery_observed",
            Self::FailurePersisted => "failure_persisted",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct RecoveryObservation {
    pub failure_ref: String,
    pub attempt_id: String,
    pub state: RecoveryState,
    pub scope: Option<EvidenceScope>,
    pub operation: Option<String>,
    pub status_code: Option<u16>,
    pub duration: Option<Duration>,
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
}
