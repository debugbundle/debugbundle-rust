use serde_json::{json, Value};
use std::collections::HashMap;
type Key = (String, u64);
struct Span {
    parent: Option<u64>,
    value: Value,
    bytes: usize,
}
#[derive(Default)]
pub(crate) struct Registry {
    entries: HashMap<Key, Span>,
    bytes: usize,
}
impl Registry {
    pub fn insert(&mut self, layer: &str, id: u64, parent: Option<u64>, value: Value) {
        if self.entries.len() >= 128 {
            return;
        }
        let Some(bytes) = crate::wire::canonical_size(&value)
            .and_then(|bytes| bytes.checked_add(layer.len() + 64))
        else {
            return;
        };
        if bytes > 262144usize.saturating_sub(self.bytes) {
            return;
        }
        let key = (layer.to_owned(), id);
        if self.entries.contains_key(&key) {
            return;
        }
        self.bytes += bytes;
        self.entries.insert(
            key,
            Span {
                parent,
                value,
                bytes,
            },
        );
    }
    pub fn update(&mut self, layer: &str, id: u64, fields: &Value) {
        let key = (layer.to_owned(), id);
        let Some(span) = self.entries.get(&key) else {
            return;
        };
        let mut value = span.value.clone();
        let Some(values) = fields.as_object() else {
            return;
        };
        let Some(output) = value["fields"].as_object_mut() else {
            return;
        };
        for (key, value) in values {
            if output.len() < 32 || output.contains_key(key) {
                output.insert(key.clone(), value.clone());
            }
        }
        let Some(bytes) = crate::wire::canonical_size(&value)
            .and_then(|bytes| bytes.checked_add(layer.len() + 64))
        else {
            return;
        };
        if bytes > 16384
            || bytes > 262144usize.saturating_sub(self.bytes.saturating_sub(span.bytes))
        {
            return;
        }
        if let Some(span) = self.entries.get_mut(&key) {
            self.bytes = self.bytes.saturating_sub(span.bytes) + bytes;
            span.value = value;
            span.bytes = bytes;
        }
    }
    pub fn snapshot(&self, layer: &str, mut id: Option<u64>) -> Value {
        let mut values = Vec::new();
        let mut seen = Vec::new();
        for _ in 0..8 {
            let Some(current) = id else {
                break;
            };
            if seen.contains(&current) {
                break;
            }
            seen.push(current);
            let Some(span) = self.entries.get(&(layer.to_owned(), current)) else {
                break;
            };
            values.push(span.value.clone());
            id = span.parent;
        }
        values.reverse();
        json!(values)
    }
    pub fn close(&mut self, layer: &str, id: u64) {
        if let Some(span) = self.entries.remove(&(layer.to_owned(), id)) {
            self.bytes = self.bytes.saturating_sub(span.bytes);
        }
    }
    pub fn detach(&mut self, layer: &str) {
        self.entries.retain(|(key, _), span| {
            if key == layer {
                self.bytes = self.bytes.saturating_sub(span.bytes);
                false
            } else {
                true
            }
        });
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}
#[cfg(test)]
mod tests;
