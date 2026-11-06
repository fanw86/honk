use super::validation::ValidationFailure;

/// JLS is mandatory and independent of the ordinary TLS/REALITY options.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowQuicConfig {
    pub username: Option<String>,
    pub password: Option<String>,
    pub server_name: Option<String>,
    pub alpn: Vec<String>,
    pub congestion: String,
    pub zero_rtt: bool,
    pub over_stream: bool,
    /// Milliseconds; omitted uses the reference client's 60-second default.
    pub half_close_timeout: Option<u64>,
    pub mtu: Option<u16>,
}

impl Default for ShadowQuicConfig {
    fn default() -> Self {
        Self {
            username: None,
            password: None,
            server_name: None,
            alpn: vec!["h3".into()],
            congestion: "bbr".into(),
            zero_rtt: true,
            over_stream: false,
            half_close_timeout: None,
            mtu: None,
        }
    }
}

impl ShadowQuicConfig {
    pub(super) fn validate(&self) -> Result<(), ValidationFailure> {
        let invalid = |field, message| ValidationFailure::new(Some(field), message);
        for (field, value) in [
            ("shadowquic_username", self.username.as_deref()),
            ("shadowquic_password", self.password.as_deref()),
        ] {
            if value.is_none_or(|value| value.trim().is_empty()) {
                return Err(invalid(field, "ShadowQUIC requires nonempty username and password"));
            }
        }
        let name = self.server_name.as_deref().unwrap_or_default();
        let dns_name = name.len() <= 253
            && name.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
        if !dns_name && name.parse::<std::net::IpAddr>().is_err() {
            return Err(invalid("shadowquic_server_name", "ShadowQUIC requires a valid server name"));
        }
        if self.alpn.is_empty()
            || self.alpn.iter().any(|name| name.is_empty() || name.len() > 255)
            || self.alpn.iter().map(|name| name.len() + 1).sum::<usize>() > 65_533
        {
            return Err(invalid("shadowquic_alpn", "ShadowQUIC requires nonempty ALPN names of 1..=255 bytes and at most 65533 encoded bytes"));
        }
        if !matches!(self.congestion.as_str(), "bbr" | "cubic" | "new_reno") {
            return Err(invalid("shadowquic_congestion", "ShadowQUIC congestion must be bbr, cubic or new_reno"));
        }
        if self.mtu.is_some_and(|mtu| !(1200..=65527).contains(&mtu)) {
            return Err(invalid("quic_mtu", "ShadowQUIC MTU must be 1200..=65527"));
        }
        Ok(())
    }
}
