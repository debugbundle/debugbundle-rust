use debugbundle::{privacy::Privacy, protocol::acknowledgement, wire};
use serde_json::{json, Value};

// A fixed bounded generator makes every failure reproducible without unbounded
// network, arbitrary callbacks or corpus growth in the ordinary test lane.
struct Generator(u64);
impl Generator {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn value(&mut self, depth: usize) -> Value {
        match self.next() % if depth == 0 { 4 } else { 6 } {
            0 => Value::Null,
            1 => json!(self.next() & 1 == 0),
            2 => json!(self.next() % 1_000_000),
            3 => json!(format!("technical é/\n\"{}", self.next() % 1000)),
            4 => Value::Array(
                (0..self.next() % 4)
                    .map(|_| self.value(depth - 1))
                    .collect(),
            ),
            _ => {
                let mut object = serde_json::Map::new();
                for index in 0..self.next() % 4 {
                    object.insert(format!("field_{index}"), self.value(depth - 1));
                }
                Value::Object(object)
            }
        }
    }
}

#[test]
fn bounded_primitive_privacy_is_owned_idempotent_and_canonically_sized() {
    let privacy = Privacy::new(&["owner_private".into()]).unwrap();
    let mut random = Generator(0xdecafbad);
    for _ in 0..1024 {
        let source = json!({"technical":random.value(7), "password":"SYNTHETIC_SECRET", "owner_private":{"nested":"OWNER_SECRET"}});
        let original = source.clone();
        let protected = privacy.protect(&source).unwrap();
        assert_eq!(source, original);
        assert_eq!(privacy.protect(&protected).unwrap(), protected);
        let bytes = serde_json::to_vec(&protected).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("SYNTHETIC_SECRET"));
        assert!(!String::from_utf8_lossy(&bytes).contains("OWNER_SECRET"));
        // This generator uses native integer representations, so serde and the
        // ECMAScript-compatible canonical wire counter have identical lengths.
        assert_eq!(wire::canonical_size(&protected), Some(bytes.len()));
        assert!(bytes.len() < wire::MAX_BODY_BYTES);
    }
}

#[test]
fn indexed_acknowledgements_partition_every_owner_and_reject_mutated_totals_or_duplicates() {
    let mut random = Generator(0xaffecafe);
    for _ in 0..1024 {
        let count = (random.next() % 100 + 1) as usize;
        let (mut accepted, mut retry, mut terminal, mut errors) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for index in 0..count {
            match random.next() % 3 {
                0 => accepted.push(index),
                1 => {
                    retry.push(index);
                    errors.push(json!({"index":index,"reason":"rate_limited"}));
                }
                _ => {
                    terminal.push(index);
                    errors.push(json!({"index":index,"reason":"invalid_event"}));
                }
            }
        }
        errors.reverse();
        let body = json!({"accepted":accepted.len(),"rejected":errors.len(),"errors":errors});
        let mut ack = acknowledgement(&body, count).unwrap();
        ack.retry.sort_unstable();
        ack.terminal.sort_unstable();
        assert_eq!(ack.accepted, accepted);
        assert_eq!(ack.retry, retry);
        assert_eq!(ack.terminal, terminal);
        let mut mutated = body.clone();
        mutated["accepted"] = json!(accepted.len() + 1);
        assert!(acknowledgement(&mutated, count).is_none());
        if errors.len() >= 2 {
            mutated = body;
            mutated["errors"][1]["index"] = mutated["errors"][0]["index"].clone();
            assert!(acknowledgement(&mutated, count).is_none());
        }
    }
}

#[test]
fn capability_subsets_have_one_canonical_order_and_never_ignore_invalid_members() {
    let mut random = Generator(0x50c0ffee);
    for _ in 0..512 {
        let mask = random.next() as usize;
        let expected = wire::FEATURES
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, feature)| (*feature).to_owned())
            .collect::<Vec<_>>();
        let mut features = expected.clone();
        for index in (1..features.len()).rev() {
            let other = random.next() as usize % (index + 1);
            features.swap(index, other);
        }
        assert_eq!(
            wire::capabilities(&json!({"version":1,"features":features})),
            expected
        );
        features.push("unsupported_future_feature".into());
        assert!(wire::capabilities(&json!({"version":1,"features":features})).is_empty());
        if let Some(first) = expected.first() {
            let mut duplicate = expected.clone();
            duplicate.push(first.clone());
            assert!(wire::capabilities(&json!({"version":1,"features":duplicate})).is_empty());
        }
    }
}
