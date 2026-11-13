//! Mandatory bounded telemetry protection; extra keys only extend the baseline.
use regex::{Captures, Regex};
use serde_json::{Map, Value};
use std::{collections::HashSet, sync::LazyLock};

pub const REDACTED: &str = "[REDACTED]";
const MAX_BYTES: usize = 256 * 1024;
const FIELDS: &[&str] = &[
    "password",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
    "private_key",
    "passwd",
    "card_number",
    "cvv",
    "cvc",
    "pin",
    "expiry",
    "phone",
    "bearer",
    "session_id",
    "otp",
    "verification_code",
    "authorization",
    "cookie",
    "ssn",
    "credit_card",
    "client_secret",
    "x_api_key",
    "accessToken",
    "refreshToken",
    "privateKey",
    "clientSecret",
    "_debug_probe",
];

struct Patterns {
    header: Regex,
    bearer: Regex,
    token: Regex,
    pem: Regex,
    pem_start: Regex,
    url: Regex,
    card: Regex,
    malformed: Regex,
    escaped: Regex,
    camel: Regex,
    separators: Regex,
}
static PATTERNS: LazyLock<Option<Patterns>> = LazyLock::new(|| {
    Some(Patterns {
    header: Regex::new(r"(?i)\b(Authorization|Proxy-Authorization|Cookie|Set-Cookie)\s*:\s*[^\r\n]*").ok()?,
    bearer: Regex::new(r"(?i)\b(Bearer|Basic)\s+[A-Za-z0-9._~+/-]{6,}").ok()?,
    token: Regex::new(r"\bdbundle_(?:proj|mem|probe|agent|invite|org_invite|org_transfer)_[A-Za-z0-9_-]+\b").ok()?,
    pem: Regex::new(r"(?i)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC |OPENSSH )?PRIVATE KEY-----").ok()?,
    pem_start: Regex::new(r"(?i)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----").ok()?,
    url: Regex::new(r#"(?i)\bhttps?://[^\s<>"']+"#).ok()?,
    card: Regex::new(r"(?:[0-9][ -]?){12,18}[0-9]").ok()?,
    malformed: Regex::new(r#"(?i)(?:password|token|secret|authorization|cookie)["']?\s*[:=]"#).ok()?,
    escaped: Regex::new(r"(?i)(?:password|token|secret|authorization|cookie)%3[ad]").ok()?,
    camel: Regex::new(r"([a-z0-9])([A-Z])").ok()?,
    separators: Regex::new(r"[^a-z0-9]+").ok()?,
})
});

#[derive(Clone)]
pub struct Privacy {
    fields: HashSet<String>,
    assignment: Regex,
}

impl Privacy {
    pub fn new(additional: &[String]) -> Option<Self> {
        if additional.len() > 128
            || additional
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 64)
        {
            return None;
        }
        PATTERNS.as_ref()?;
        let labels: Vec<_> = FIELDS
            .iter()
            .map(|s| (*s).to_owned())
            .chain(additional.iter().cloned())
            .collect();
        let fields = labels.iter().map(|s| canonical_key(s)).collect();
        let names = labels
            .iter()
            .map(|s| regex::escape(s))
            .collect::<Vec<_>>()
            .join("|");
        let assignment = Regex::new(&format!(
            r#"(?i)\b({names})\b(["']?\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s&,;]+)"#
        ))
        .ok()?;
        Some(Self { fields, assignment })
    }

    pub fn protect(&self, value: &Value) -> Option<Value> {
        let mut budget = Budget { nodes: 0, bytes: 0 };
        let output = self.visit(value, 0, true, &mut budget)?;
        (serde_json::to_vec(&output).ok()?.len() <= MAX_BYTES).then_some(output)
    }

    /// The protocol permits 500 probe items, while application containers remain
    /// limited to 256 entries. Optional diagnostics never consume primary data's
    /// budget or force an otherwise safe failure to be discarded.
    pub(crate) fn protect_payload(&self, value: &Value) -> Option<Value> {
        let Some(map) = value
            .as_object()
            .filter(|map| map.contains_key("probe_data"))
        else {
            return self.protect(value);
        };
        if map.len() > 256 {
            return None;
        }
        let mut budget = Budget { nodes: 1, bytes: 0 };
        let mut output = Map::new();
        for (key, nested) in map {
            if key == "probe_data" || key.len() > 128 {
                continue;
            }
            budget.charge(key.len())?;
            if self.scrub(key, true)? != *key {
                continue;
            }
            let cleaned = if self.sensitive_key(key) {
                Value::String(REDACTED.into())
            } else {
                self.visit(nested, 1, true, &mut budget)?
            };
            output.insert(key.clone(), cleaned);
        }
        let probes = (|| {
            let probes = value["probe_data"].as_object()?;
            if probes.len() != 2 || probes.get("version")? != 1 {
                return None;
            }
            let items = probes.get("items")?.as_array()?;
            if items.is_empty() || items.len() > 500 {
                return None;
            }
            let mut optional = budget;
            optional.charge(32)?;
            let items = items
                .iter()
                .map(|item| self.visit(item, 3, true, &mut optional))
                .collect::<Option<Vec<_>>>()?;
            if items.iter().any(|item| !item["data"].is_object()) {
                return None;
            }
            Some(serde_json::json!({"version":1,"items":items}))
        })();
        if let Some(probes) = probes {
            output.insert("probe_data".into(), probes);
        }
        let output = Value::Object(output);
        (serde_json::to_vec(&output).ok()?.len() <= MAX_BYTES).then_some(output)
    }

    pub fn text(&self, text: &str) -> String {
        if text.len() > 16 * 1024 {
            return REDACTED.into();
        }
        self.scrub(text, true).unwrap_or_else(|| REDACTED.into())
    }

    pub fn sensitive_key(&self, key: &str) -> bool {
        // Public helper calls receive the same bound as object/query traversal.
        // Never allocate or run the segment matcher for an unchecked long name.
        if key.len() > 128 {
            return true;
        }
        let Some(p) = PATTERNS.as_ref() else {
            return true;
        };
        let expanded = p.camel.replace_all(key.trim(), "${1}_${2}").to_lowercase();
        let parts: Vec<_> = p.separators.split(&expanded).collect();
        for start in 0..parts.len() {
            let mut joined = String::new();
            for part in &parts[start..] {
                joined.push_str(part);
                if self.fields.contains(&joined) {
                    return true;
                }
            }
        }
        false
    }

    fn visit(
        &self,
        value: &Value,
        depth: usize,
        structured: bool,
        budget: &mut Budget,
    ) -> Option<Value> {
        budget.nodes += 1;
        if budget.nodes > 4096 {
            return None;
        }
        if depth > 16 {
            return Some(Value::String(REDACTED.into()));
        }
        match value {
            Value::String(s) => {
                if s.len() > 16 * 1024 {
                    return Some(Value::String(REDACTED.into()));
                }
                budget.charge(s.len())?;
                if structured && (s.starts_with('{') || s.starts_with('[')) {
                    if let Ok(nested @ (Value::Object(_) | Value::Array(_))) =
                        serde_json::from_str::<Value>(s)
                    {
                        let cleaned = self.visit(&nested, depth + 1, false, budget)?;
                        return Some(Value::String(serde_json::to_string(&cleaned).ok()?));
                    }
                }
                let output = self.scrub(s, true)?;
                if output == *s
                    && (s.starts_with('{') || s.starts_with('['))
                    && PATTERNS.as_ref()?.malformed.is_match(s)
                {
                    return Some(Value::String(REDACTED.into()));
                }
                Some(Value::String(output))
            }
            Value::Object(values) => {
                if values.len() > 256 {
                    return Some(Value::String(REDACTED.into()));
                }
                let mut output = Map::new();
                for (key, nested) in values {
                    if key.len() > 128 {
                        continue;
                    }
                    budget.charge(key.len())?;
                    if self.scrub(key, true)? != *key {
                        continue;
                    }
                    let cleaned = if self.sensitive_key(key) {
                        Value::String(REDACTED.into())
                    } else {
                        self.visit(nested, depth + 1, structured, budget)?
                    };
                    output.insert(key.clone(), cleaned);
                }
                Some(Value::Object(output))
            }
            Value::Array(values) => {
                if values.len() > 256 {
                    return Some(Value::String(REDACTED.into()));
                }
                values
                    .iter()
                    .map(|v| self.visit(v, depth + 1, structured, budget))
                    .collect::<Option<Vec<_>>>()
                    .map(Value::Array)
            }
            Value::Number(number) => {
                budget.charge(crate::wire::number_size(number)?)?;
                Some(value.clone())
            }
            _ => Some(value.clone()),
        }
    }

    fn scrub(&self, input: &str, scan_urls: bool) -> Option<String> {
        let p = PATTERNS.as_ref()?;
        if p.pem_start.is_match(input) && !p.pem.is_match(input) {
            return Some(REDACTED.into());
        }
        let mut output = if p.escaped.is_match(input) {
            strict_decode(input, false)?
        } else {
            input.to_owned()
        };
        output = p.pem.replace_all(&output, REDACTED).into_owned();
        output = p
            .header
            .replace_all(&output, "${1}: [REDACTED]")
            .into_owned();
        output = p
            .bearer
            .replace_all(&output, "${1} [REDACTED]")
            .into_owned();
        output = p.token.replace_all(&output, REDACTED).into_owned();
        output = self
            .assignment
            .replace_all(&output, "${1}${2}[REDACTED]")
            .into_owned();
        let original = output.clone();
        output = p
            .card
            .replace_all(&original, |cap: &Captures<'_>| {
                let Some(found) = cap.get(0) else {
                    return String::new();
                };
                let before = original[..found.start()].bytes().next_back();
                let after = original[found.end()..].bytes().next();
                if before.is_some_and(identifier)
                    || after.is_some_and(identifier)
                    || !valid_card(found.as_str())
                {
                    found.as_str().to_owned()
                } else {
                    REDACTED.into()
                }
            })
            .into_owned();
        if scan_urls {
            output = p
                .url
                .replace_all(&output, |cap: &Captures<'_>| {
                    let candidate = cap[0].trim_end_matches([')', '.', ',', ';']);
                    let suffix = &cap[0][candidate.len()..];
                    format!(
                        "{}{suffix}",
                        self.scrub_url(candidate).unwrap_or_else(|| REDACTED.into())
                    )
                })
                .into_owned();
        }
        Some(output)
    }

    fn scrub_url(&self, input: &str) -> Option<String> {
        let mut parsed = url::Url::parse(input).ok()?;
        parsed.host_str()?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            parsed.set_username("REDACTED").ok()?;
            parsed.set_password(None).ok()?;
        }
        let mut pairs = Vec::new();
        if let Some(query) = parsed.query() {
            for pair in query.split('&') {
                let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
                let key = strict_decode(raw_key, true)?;
                let mut value = strict_decode(raw_value, true)?;
                if key.len() > 128 || self.scrub(&key, false)? != key {
                    continue;
                }
                if self.sensitive_key(&key) || self.scrub(&value, false)? != value {
                    value = REDACTED.into();
                }
                pairs.push((key, value));
            }
        }
        parsed.set_query(None);
        if !pairs.is_empty() {
            parsed.query_pairs_mut().extend_pairs(pairs);
        }
        parsed.set_fragment(None);
        Some(parsed.into())
    }
}

struct Budget {
    nodes: usize,
    bytes: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize) -> Option<()> {
        self.bytes = self.bytes.checked_add(bytes)?;
        (self.bytes <= MAX_BYTES).then_some(())
    }
}
fn canonical_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn identifier(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}
fn strict_decode(s: &str, plus: bool) -> Option<String> {
    for (i, b) in s.bytes().enumerate() {
        if b == b'%'
            && (!s.as_bytes().get(i + 1)?.is_ascii_hexdigit()
                || !s.as_bytes().get(i + 2)?.is_ascii_hexdigit())
        {
            return None;
        }
    }
    let input = if plus {
        s.replace('+', " ")
    } else {
        s.to_owned()
    };
    percent_encoding::percent_decode_str(&input)
        .decode_utf8()
        .ok()
        .map(|s| s.into_owned())
}
fn valid_card(s: &str) -> bool {
    let digits: Vec<_> = s
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    if !(13..=19).contains(&digits.len()) || digits.iter().all(|d| *d == digits[0]) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let n = u32::from(*d) * if i % 2 == 1 { 2 } else { 1 };
            if n > 9 {
                n - 9
            } else {
                n
            }
        })
        .sum();
    sum.is_multiple_of(10)
}
