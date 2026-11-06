use super::options::{Query, query_error};
use crate::diagnostic::SourceRef;
use crate::error::DetailedConfigError;
use crate::node::ShadowQuicConfig;

fn value<'a>(query: &'a Query, names: &[&'static str], source: &SourceRef) -> Result<Option<&'a str>, DetailedConfigError> {
    let mut selected = None;
    for &name in names {
        for value in query.values(name) {
            if selected.is_some_and(|previous| previous != value) {
                return Err(query_error(source, &["shadowquic"], "invalid-config-value", "ShadowQUIC aliases and duplicate parameters must agree"));
            }
            selected = Some(value);
        }
    }
    Ok(selected)
}

pub(super) fn check_query(query: &Query, source: &SourceRef) -> Result<(), DetailedConfigError> {
    for (key, _) in &query.0 {
        if !matches!(key.as_str(), "sni" | "peer" | "alpn" | "congestion" | "zero-rtt" | "over-stream" | "half-close-timeout" | "mtu" | "remark") {
            return Err(query_error(source, &["shadowquic"], "invalid-config-value", "unsupported ShadowQUIC share-link option; JLS does not use ordinary TLS or stream options"));
        }
    }
    Ok(())
}

pub(super) fn apply(config: &mut ShadowQuicConfig, query: &Query, source: &SourceRef) -> Result<(), DetailedConfigError> {
    let invalid = || query_error(source, &["shadowquic"], "invalid-config-value", "invalid ShadowQUIC option value");
    config.server_name = value(query, &["sni", "peer"], source)?.map(str::to_string);
    if let Some(alpn) = value(query, &["alpn"], source)? {
        config.alpn = alpn.split(',').map(|name| name.trim().to_string()).collect();
    }
    if let Some(congestion) = value(query, &["congestion"], source)? {
        config.congestion = congestion.to_string();
    }
    for (name, destination) in [("zero-rtt", &mut config.zero_rtt), ("over-stream", &mut config.over_stream)] {
        if let Some(value) = value(query, &[name], source)? {
            *destination = match value {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => return Err(invalid()),
            };
        }
    }
    config.mtu = value(query, &["mtu"], source)?.map(|value| value.parse().map_err(|_| invalid())).transpose()?;
    config.half_close_timeout = value(query, &["half-close-timeout"], source)?.map(|value| value.parse().map_err(|_| invalid())).transpose()?;
    Ok(())
}
