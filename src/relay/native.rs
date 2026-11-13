use super::{RelayConnection, RelayRequest, RelayResponse};

pub(crate) const BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const MAX_FRAMES: usize = 4096;

/// Copy only the bounded ingress fields. Invalid/duplicate fields cannot silently
/// become absent and enable a less strict fallback at the trust boundary.
pub(crate) fn head<'a>(
    method: &str,
    connection: RelayConnection,
    query: Option<&str>,
    mut header: impl FnMut(&str) -> Result<Option<&'a str>, ()>,
) -> Result<(RelayRequest, Option<u64>), RelayResponse> {
    let mut field = |name: &str, maximum: usize| {
        header(name).and_then(|value| {
            value.map_or(Ok(None), |value| {
                (value.len() <= maximum)
                    .then(|| Some(value.to_owned()))
                    .ok_or(())
            })
        })
    };
    let mut build = || -> Result<_, ()> {
        if method.len() > 16 || query.is_some_and(|query| query.len() > 8192) {
            return Err(());
        }
        let length = field("content-length", 20)?
            .map(|value| {
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(());
                }
                value.parse::<u64>().map_err(|_| ())
            })
            .transpose()?;
        let evidence = field("x-debugbundle-evidence", 16)?.as_deref() == Some("1")
            || query.is_some_and(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .any(|(key, value)| key == "debugbundle_evidence" && value == "1")
            });
        Ok((
            RelayRequest {
                method: method.to_owned(),
                scheme: if connection.secure { "https" } else { "http" }.into(),
                host: field("host", 2048)?.ok_or(())?,
                origin: field("origin", 8192)?,
                referer: field("referer", 8192)?,
                content_type: field("content-type", 128)?,
                peer: connection.peer,
                forwarded_host: field("x-forwarded-host", 2048)?,
                forwarded_proto: field("x-forwarded-proto", 16)?,
                forwarded_for: field("x-forwarded-for", 45)?,
                evidence,
            },
            length,
        ))
    };
    build().map_err(|()| RelayResponse::status(400))
}
