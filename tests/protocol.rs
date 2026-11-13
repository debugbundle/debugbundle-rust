use debugbundle::protocol::{acknowledgement, retry_after};
use serde_json::json;
use std::time::{Duration, SystemTime};

#[test]
fn partial_acknowledgements_reconcile_each_index_once() {
    let result=acknowledgement(&json!({"accepted":1,"rejected":2,"errors":[{"index":1,"reason":"rate_limited"},{"index":2,"reason":"invalid_event"}]}),3).unwrap();
    assert_eq!(result.accepted, vec![0]);
    assert_eq!(result.retry, vec![1]);
    assert_eq!(result.terminal, vec![2]);
    for reason in ["monthly_quota_exceeded", "analytics_quota_exceeded"] {
        assert_eq!(
            acknowledgement(
                &json!({"accepted":0,"rejected":1,"errors":[{"index":0,"reason":reason}]}),
                1
            )
            .unwrap()
            .retry,
            vec![0]
        );
    }
}

#[test]
fn json_mathematical_integers_share_the_receiving_contract() {
    let body = serde_json::from_str(
        r#"{"accepted":1.0,"rejected":1e0,"errors":[{"index":1.0,"reason":"rate_limited"}]}"#,
    )
    .unwrap();
    let ack = acknowledgement(&body, 2).unwrap();
    assert_eq!(ack.accepted, vec![0]);
    assert_eq!(ack.retry, vec![1]);
    for accepted in ["0.5", "-1.0", "1e10000"] {
        let body: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"accepted":{accepted},"rejected":0,"errors":[]}}"#
        ))
        .unwrap();
        assert!(acknowledgement(&body, 1).is_none());
    }
}

#[test]
fn malformed_acknowledgements_cannot_release_ownership() {
    for value in [
        json!({}),
        json!({"accepted":1,"rejected":0}),
        json!({"accepted":1,"rejected":1,"errors":[]}),
        json!({"accepted":-1,"rejected":2,"errors":[]}),
        json!({"accepted":18446744073709551615u64,"rejected":1,"errors":[]}),
        json!({"accepted":0,"rejected":1,"errors":[{"index":1,"reason":"rate_limited"}]}),
        json!({"accepted":0,"rejected":1,"errors":[{"index":0,"reason":" "}]}),
        json!({"accepted":0,"rejected":2,"errors":[{"index":0,"reason":"x"},{"index":0,"reason":"x"}]}),
    ] {
        assert!(acknowledgement(&value, 1).is_none(), "{value}");
    }
}

#[test]
fn retry_hints_are_capped_before_deadline_arithmetic() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000000);
    assert_eq!(
        retry_after("1844674407370955161500000000000", now),
        Some(Duration::from_secs(300))
    );
    assert_eq!(retry_after("7", now), Some(Duration::from_secs(7)));
    assert_eq!(
        retry_after(
            &httpdate::fmt_http_date(now + Duration::from_secs(301)),
            now
        ),
        Some(Duration::from_secs(300))
    );
    assert_eq!(
        retry_after(&httpdate::fmt_http_date(now - Duration::from_secs(1)), now),
        Some(Duration::ZERO)
    );
    for value in ["-1", "NaN", "infinity", "", "3.5"] {
        assert!(retry_after(value, now).is_none());
    }
}
