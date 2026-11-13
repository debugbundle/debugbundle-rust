use super::*;
#[test]
fn span_registry_has_instance_wide_count_bytes_and_close_bounds() {
    let mut registry = Registry::default();
    for id in 0..130 {
        registry.insert("layer", id, None, json!({"fields":{},"name":"technical"}));
    }
    assert_eq!(registry.entries.len(), 128);
    registry.close("layer", 0);
    registry.insert("another", 0, None, json!({"fields":{},"name":"other"}));
    assert_eq!(registry.entries.len(), 128);
    registry.detach("layer");
    assert_eq!(registry.entries.len(), 1);
    registry.detach("another");
    assert_eq!(registry.bytes, 0);
}
