use super::{capture, Client};
use crate::{Context, EvidenceScope, RecoveryObservation};
use serde_json::json;
impl Client {
    pub fn capture_recovery(&self, observation: &RecoveryObservation, context: &Context) {
        let Some(inner) = &self.inner else {
            return;
        };
        if !inner.config.capture_recovery_observations
            || !super::probes::valid_context(inner, context)
        {
            return;
        }
        if crate::operations::trace_id(&observation.failure_ref).is_none()
            || crate::operations::trace_id(&observation.attempt_id).is_none()
            || observation
                .operation
                .as_ref()
                .is_some_and(|label| label.len() > 64)
            || observation
                .status_code
                .is_some_and(|status| !(100..=599).contains(&status))
        {
            return;
        }
        self.enqueue(0,|state|state.policy.diagnostics&&state.features.iter().any(|feature|feature=="recovery_v1"),|inner,state|{
            let context=capture::effective(inner,context);
            let scope=if let Some(scope)=&observation.scope{scope.value()?}else{
                let operation=context.operation.as_ref().filter(|operation|operation.matches(&inner.operations))?;
                EvidenceScope::Operation{operation_id:operation.operation_id()?.into()}.value()?
            };
            if let Some(operation)=context.operation.as_ref(){if !operation.matches(&inner.operations)||(scope["kind"]=="operation"&&scope["operation_id"].as_str()!=operation.operation_id()){return None;}}
            let mut payload=json!({"version":1,"kind":"recovery","failure_ref":observation.failure_ref,"scope":scope,"attempt_id":observation.attempt_id,"state":observation.state.as_str()});
            if let Some(label)=&observation.operation{payload["operation"]=json!(inner.privacy.text(label));}
            if let Some(status)=observation.status_code{payload["status_code"]=json!(status);}
            if let Some(duration)=observation.duration{payload["duration_ms"]=json!(duration.as_secs_f64()*1000.0);}
            let mut event=capture::base(inner,state,"diagnostic_context",payload,&context)?;
            if let Some(at)=observation.occurred_at{event["occurred_at"]=json!(at.to_rfc3339_opts(chrono::SecondsFormat::Millis,true));}
            Some(event)
        });
    }
}
