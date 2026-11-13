//! Fallible OS randomness: entropy failure disables capture instead of panicking.
pub(crate) fn fresh() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Some(uuid::Uuid::from_bytes(bytes).to_string())
}
