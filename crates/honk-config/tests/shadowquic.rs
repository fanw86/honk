use honk_config::node::Node;
use honk_config::types::NodeProtocol;

const LINK: &str = "shadowquic://user:secret@proxy.example:443?sni=cover.example#node";

#[test]
fn shadowquic_defaults_and_credentials() {
    let node = Node::from_share_link(LINK).unwrap();
    assert_eq!(node.protocol(), NodeProtocol::ShadowQuic);
    assert_eq!(
        "shadowquic".parse::<NodeProtocol>().unwrap(),
        node.protocol()
    );
    assert_eq!(node.protocol().as_str(), "shadowquic");
    let config = node.shadowquic().unwrap();
    assert_eq!(config.username.as_deref(), Some("user"));
    assert_eq!(config.password.as_deref(), Some("secret"));
    assert_eq!(config.server_name.as_deref(), Some("cover.example"));
    assert_eq!(config.alpn, ["h3"]);
    assert_eq!(config.congestion, "bbr");
    assert!(config.zero_rtt);
    assert!(!config.over_stream);
    assert!(node.tls().is_none());
    let ipv6 =
        Node::from_share_link("shadowquic://u%3Aname:p%40ss@[::1]:1443?peer=cover.example#ipv6")
            .unwrap();
    assert_eq!(
        ipv6.shadowquic().unwrap().username.as_deref(),
        Some("u:name")
    );
    assert_eq!(ipv6.shadowquic().unwrap().password.as_deref(), Some("p@ss"));
    assert_eq!(ipv6.port, 1443);
}

#[test]
fn shadowquic_uri_rejects_bad_options_without_exposing_values() {
    for suffix in [
        "sni=",
        "sni=bad%20name",
        "sni=a&sni=b",
        "sni=a&peer=b",
        "sni=a&%73ni=b",
        "sni=a&alpn=",
        "sni=a&alpn=h3,",
        "sni=a&congestion=private-value",
        "sni=a&zero-rtt=yes",
        "sni=a&zero-rtt=1&zero-rtt=0",
        "sni=a&over-stream=2",
        "sni=a&half-close-timeout=-1",
        "sni=a&half-close-timeout=NaN",
        "sni=a&mtu=1199",
        "sni=a&mtu=65528",
        "sni=a&mtu=no",
        "sni=a&tls=0",
        "sni=a&security=reality",
        "sni=a&pbk=private-value",
        "sni=a&allowInsecure=1",
        "sni=a&type=ws",
        "sni=a&unknown=private-value",
    ] {
        let link = format!(
            "shadowquic://private-user:private-password@private-host:443?{suffix}#private-name"
        );
        let mut diagnostics = Vec::new();
        let error =
            Node::from_share_link_with_detailed_diagnostics(&link, &mut diagnostics).unwrap_err();
        let rendered = format!("{error:?} {diagnostics:?}");
        for private in [
            "private-user",
            "private-password",
            "private-host",
            "private-name",
            "private-value",
        ] {
            assert!(!rendered.contains(private), "diagnostic leaked {private}");
        }
    }
    for link in [
        "shadowquic://u:p@h",
        "shadowquic://u@h?sni=a",
        "shadowquic://:p@h?sni=a",
    ] {
        assert!(Node::from_share_link(link).is_err());
    }
}

#[test]
fn shadowquic_effective_defaults_and_tuning_preserve_identity() {
    let node = Node::from_share_link(LINK).unwrap();
    let explicit = Node::from_share_link("shadowquic://user:secret@proxy.example:443?sni=cover.example&alpn=h3&congestion=bbr&zero-rtt=true&over-stream=false&mtu=1300&half-close-timeout=60000#renamed").unwrap();
    assert_eq!(node.id, explicit.id);
    for (field, value) in [
        ("sni", "other.example"),
        ("alpn", "h3,h2"),
        ("congestion", "cubic"),
        ("zero-rtt", "0"),
        ("over-stream", "1"),
    ] {
        let query = if field == "sni" {
            format!("sni={value}")
        } else {
            format!("sni=cover.example&{field}={value}")
        };
        let changed = Node::from_share_link(&format!(
            "shadowquic://user:secret@proxy.example:443?{query}#node"
        ))
        .unwrap();
        assert_ne!(node.id, changed.id, "{field}");
    }
    let a = Node::from_share_link("shadowquic://a%7Cb:c@h?sni=a").unwrap();
    let b = Node::from_share_link("shadowquic://a:b%7Cc@h?sni=a").unwrap();
    assert_ne!(a.id, b.id);
}

#[test]
fn shadowquic_flat_roundtrips_and_alias_conflicts() {
    let node = Node::from_share_link("shadowquic://u:p@h?sni=a&alpn=h3,h2&congestion=new_reno&zero-rtt=0&over-stream=1&mtu=1400&half-close-timeout=42#node").unwrap();
    let restored: Node = serde_json::from_value(serde_json::to_value(&node).unwrap()).unwrap();
    assert_eq!(node.outbound, restored.outbound);
    assert_eq!(node.derive_id(), restored.derive_id());
    let yaml: Node = serde_yaml::from_str(&serde_yaml::to_string(&node).unwrap()).unwrap();
    let toml: Node = toml::from_str(&toml::to_string(&node).unwrap()).unwrap();
    assert_eq!(node.outbound, yaml.outbound);
    assert_eq!(node.outbound, toml.outbound);
    let mut flat = serde_json::to_value(&node).unwrap();
    flat["sni"] = "different.example".into();
    assert!(serde_json::from_value::<Node>(flat).is_err());
    for field in ["shadowquic_username", "shadowquic_password"] {
        let mut flat = serde_json::to_value(&node).unwrap();
        flat[field] = "conflict".into();
        assert!(serde_json::from_value::<Node>(flat).is_err());
    }
}

#[test]
fn shadowquic_direct_construction_validates_alpn_and_mtu() {
    let base = Node::from_share_link(LINK).unwrap();
    for alpn in [
        vec![],
        vec![String::new()],
        vec!["a".repeat(256)],
        vec!["a".repeat(255); 256],
    ] {
        let mut node = base.clone();
        node.shadowquic_mut().unwrap().alpn = alpn;
        assert!(node.validate().is_err());
    }
    for mtu in [1200, 65527] {
        let mut node = base.clone();
        node.shadowquic_mut().unwrap().mtu = Some(mtu);
        node.validate().unwrap();
    }
    let mut a = base.clone();
    let mut b = base;
    a.shadowquic_mut().unwrap().alpn = vec!["h3".into(), "h2".into()];
    b.shadowquic_mut().unwrap().alpn = vec!["h2".into(), "h3".into()];
    assert_ne!(a.derive_id(), b.derive_id());
}

#[test]
fn shadowquic_fields_do_not_load_on_other_protocols() {
    let socks = Node::from_share_link("socks5://127.0.0.1:1080#socks").unwrap();
    let mut flat = serde_json::to_value(socks).unwrap();
    flat["shadowquic_over_stream"] = false.into();
    assert!(serde_json::from_value::<Node>(flat).is_err());
    let sq = Node::from_share_link(LINK).unwrap();
    for (field, value) in [
        ("tls", serde_json::json!(true)),
        ("reality_public_key", serde_json::json!("key")),
        ("tls_alpn", serde_json::json!(["h3"])),
        ("transport", serde_json::json!("ws")),
    ] {
        let mut flat = serde_json::to_value(&sq).unwrap();
        flat[field] = value;
        assert!(serde_json::from_value::<Node>(flat).is_err());
    }
}

#[test]
fn shadowquic_dae_entry_uses_the_shared_parser() {
    let text = format!("node {{\n  '{LINK}'\n}}\n");
    let config = honk_config::parser::parse_dae_config(&text).unwrap();
    assert_eq!(config.nodes[0].protocol(), NodeProtocol::ShadowQuic);
}
