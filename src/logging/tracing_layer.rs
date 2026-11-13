use super::{enter, entered, NativeLog};
use crate::{Client, Level};
use serde_json::{json, Map, Value};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    Event, Subscriber,
};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};
struct Collector {
    client: Client,
    id: String,
}
#[derive(Default)]
struct Stack {
    entries: Vec<(String, u64)>,
    overflow: usize,
}
thread_local! {static STACK:std::cell::RefCell<Stack>=std::cell::RefCell::new(Stack::default());}
fn current(layer: &str) -> Option<u64> {
    STACK.with(|stack| {
        let stack = stack.try_borrow().ok()?;
        if stack.overflow > 0 {
            return None;
        }
        stack
            .entries
            .iter()
            .rev()
            .find(|(key, _)| key == layer)
            .map(|(_, id)| *id)
    })
}
impl Drop for Collector {
    fn drop(&mut self) {
        self.client.detach_logging_spans(&self.id);
        STACK.with(|stack| {
            if let Ok(mut stack) = stack.try_borrow_mut() {
                stack.entries.retain(|(key, _)| key != &self.id);
            }
        });
    }
}
fn level(level: &tracing::Level) -> Level {
    match *level {
        tracing::Level::ERROR => Level::Error,
        tracing::Level::WARN => Level::Warning,
        tracing::Level::INFO => Level::Info,
        tracing::Level::DEBUG => Level::Debug,
        _ => Level::Trace,
    }
}
impl Client {
    pub fn tracing_layer<S>(&self) -> impl Layer<S> + Send + Sync + 'static
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        let client = self.clone();
        Collector {
            client: self.clone(),
            id: crate::identity::fresh().unwrap_or_default(),
        }
        .with_filter(tracing_subscriber::filter::dynamic_filter_fn(
            move |metadata, _context| {
                !entered() && client.native_logging_enabled(level(metadata.level()))
            },
        ))
    }
}
#[derive(Default)]
struct Fields {
    values: Map<String, Value>,
    bytes: usize,
    native_error: bool,
}
impl Fields {
    fn insert(&mut self, field: &Field, value: Value) {
        let name = field.name();
        if name.len() > 128 || self.values.len() >= 32 {
            return;
        }
        let bytes = crate::wire::canonical_size(&value).unwrap_or(8193) + name.len();
        if bytes > 8192usize.saturating_sub(self.bytes) {
            return;
        }
        self.bytes += bytes;
        self.values.insert(name.into(), value);
    }
}
impl Visit for Fields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, json!(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, json!(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(
            field,
            if value.is_finite() {
                json!(value)
            } else {
                json!("[unavailable]")
            },
        );
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, json!(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, json!(crate::model::text(value, 2048)));
    }
    fn record_debug(&mut self, field: &Field, _: &dyn std::fmt::Debug) {
        self.insert(field, json!("[unavailable without formatting]"));
    }
    fn record_error(&mut self, field: &Field, _: &(dyn std::error::Error + 'static)) {
        self.native_error = true;
        self.insert(
            field,
            json!("[error details unavailable without a safe snapshot]"),
        );
    }
}
impl<S: Subscriber> Layer<S> for Collector {
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
        if self.id.is_empty()
            || !self
                .client
                .native_logging_enabled(level(attributes.metadata().level()))
        {
            return;
        }
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let metadata = attributes.metadata();
        let parent = attributes.parent().map(Id::into_u64).or_else(|| {
            attributes
                .is_contextual()
                .then(|| current(&self.id))
                .flatten()
        });
        self.client.record_logging_span(&self.id,id.into_u64(),parent,&json!({"name":crate::model::text(metadata.name(),256),"target":crate::model::text(metadata.target(),256),"fields":fields.values}));
    }
    fn on_record(&self, id: &Id, record: &Record<'_>, _: Context<'_, S>) {
        let mut fields = Fields::default();
        record.record(&mut fields);
        self.client
            .update_logging_span(&self.id, id.into_u64(), &Value::Object(fields.values));
    }
    fn on_enter(&self, id: &Id, _: Context<'_, S>) {
        STACK.with(|stack| {
            if let Ok(mut stack) = stack.try_borrow_mut() {
                if stack.entries.len() >= 32 || stack.overflow > 0 {
                    stack.overflow = stack.overflow.saturating_add(1);
                } else {
                    stack.entries.push((self.id.clone(), id.into_u64()));
                }
            }
        });
    }
    fn on_exit(&self, id: &Id, _: Context<'_, S>) {
        STACK.with(|stack| {
            if let Ok(mut stack) = stack.try_borrow_mut() {
                if stack.overflow > 0 {
                    stack.overflow -= 1;
                } else if let Some(index) = stack
                    .entries
                    .iter()
                    .rposition(|(layer, span)| layer == &self.id && *span == id.into_u64())
                {
                    stack.entries.remove(index);
                }
            }
        });
    }
    fn on_close(&self, id: Id, _: Context<'_, S>) {
        self.client.close_logging_span(&self.id, id.into_u64());
    }
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let Some(_guard) = enter() else {
            return;
        };
        let metadata = event.metadata();
        let level = level(metadata.level());
        if !self.client.native_logging_enabled(level) {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let message = fields
            .values
            .remove("message")
            .and_then(|value| {
                value
                    .as_str()
                    .filter(|value| *value != "[unavailable without formatting]")
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "Log message unavailable without formatting".into());
        self.client.capture_native_log(&NativeLog{level,message,attributes:Value::Object(fields.values),metadata:json!({"system":"tracing","target":crate::model::text(metadata.target(),256),"module":metadata.module_path().map(|value|crate::model::text(value,256)),"file":metadata.file().map(|value|crate::model::text(value,2048)),"line":metadata.line(),"spans":self.client.snapshot_logging_spans(&self.id,event.parent().map(Id::into_u64).or_else(||event.is_contextual().then(||current(&self.id)).flatten()))}),native_error:fields.native_error});
    }
}
