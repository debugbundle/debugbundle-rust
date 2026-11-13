//! Dispose rejected callback-owned trees without serde_json's recursive Drop.
use serde_json::Value;
pub(crate) fn discard(value: Value) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(mut values) => {
                if pending.is_empty() {
                    pending = values;
                } else {
                    pending.append(&mut values);
                }
            }
            Value::Object(values) => pending.extend(values.into_iter().map(|(_, value)| value)),
            _ => {}
        }
    }
}
