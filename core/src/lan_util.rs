//! Pure same-LAN endpoint and network utilities.

use data_encoding::{BASE64URL, BASE64URL_NOPAD};
use sha2::{Digest, Sha256};

const LAN_LINK_PREFIX: &str = "CMLAN1:";
const LAN_LINK_BASE: &str = "https://cruisemesh.app/lan#";
pub const LAN_ENDPOINT_CACHE_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
pub const LAN_ENDPOINT_RESEND_INTERVAL_MS: i64 = 5 * 60 * 1_000;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreLanEndpoint {
    pub host: String,
    pub port: u16,
}

#[uniffi::export]
pub fn core_format_lan_endpoint(endpoint: CoreLanEndpoint) -> String {
    if endpoint.host.contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    }
}

#[uniffi::export]
pub fn core_parse_lan_endpoint(text: String, default_port: u16) -> Option<CoreLanEndpoint> {
    let value = text.trim();
    if value.is_empty() {
        return None;
    }
    let (host, port_text) = if let Some(after_open) = value.strip_prefix('[') {
        let closing = after_open.find(']')?;
        if closing == 0 {
            return None;
        }
        let host = &after_open[..closing];
        let suffix = &after_open[closing + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').filter(|value| !value.is_empty())?)
        };
        (host, port)
    } else if value.matches(':').count() == 1 {
        let (host, port) = value.split_once(':')?;
        (host, Some(port))
    } else {
        // An unbracketed IPv6 address uses the default port.
        (value, None)
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    let port = match port_text {
        Some(value) => value.parse::<u16>().ok().filter(|port| *port > 0)?,
        None if default_port > 0 => default_port,
        None => return None,
    };
    Some(CoreLanEndpoint {
        host: host.to_string(),
        port,
    })
}

#[uniffi::export]
pub fn core_make_lan_endpoint_link(endpoint: CoreLanEndpoint) -> String {
    let host = BASE64URL_NOPAD.encode(endpoint.host.as_bytes());
    format!("{LAN_LINK_BASE}{LAN_LINK_PREFIX}{host}:{}", endpoint.port)
}

#[uniffi::export]
pub fn core_parse_lan_endpoint_link(fragment: Option<String>) -> Option<CoreLanEndpoint> {
    let payload = fragment?.strip_prefix(LAN_LINK_PREFIX)?.to_string();
    let (encoded_host, port) = payload.rsplit_once(':')?;
    let host = String::from_utf8(BASE64URL_NOPAD.decode(encoded_host.as_bytes()).ok()?).ok()?;
    let endpoint = core_parse_lan_endpoint(format!("[{host}]:{port}"), 1)?;
    Some(endpoint)
}

#[uniffi::export]
pub fn core_lan_network_id_for_ipv4(address: String) -> Option<String> {
    let octets = parse_ipv4(&address)?;
    let prefix = format!("{}.{}.{}.0/24", octets[0], octets[1], octets[2]);
    core_lan_network_id_for_components(vec![format!("ipv4:{prefix}")])
}

#[uniffi::export]
pub fn core_lan_network_id_for_components(components: Vec<String>) -> Option<String> {
    if components.is_empty() {
        return None;
    }
    let digest =
        Sha256::digest(format!("CruiseMesh LAN network v1\0{}", components.join("|")).as_bytes());
    Some(BASE64URL.encode(&digest[..16]))
}

#[uniffi::export]
pub fn core_subnet_24_hosts(address: String) -> Vec<String> {
    let Some(octets) = parse_ipv4(&address) else {
        return Vec::new();
    };
    (1..=254)
        .filter(|last| *last != octets[3] as u16)
        .map(|last| format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], last))
        .collect()
}

#[uniffi::export]
pub fn lan_endpoint_cache_is_fresh(saved_at_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(saved_at_ms) <= LAN_ENDPOINT_CACHE_MAX_AGE_MS
}

#[uniffi::export]
pub fn should_resend_lan_endpoint(
    previous_signature: Option<String>,
    previous_sent_at_ms: Option<i64>,
    current_signature: String,
    now_ms: i64,
) -> bool {
    previous_signature.as_deref() != Some(current_signature.as_str())
        || previous_sent_at_ms
            .map(|sent| now_ms.saturating_sub(sent) >= LAN_ENDPOINT_RESEND_INTERVAL_MS)
            .unwrap_or(true)
}

fn parse_ipv4(address: &str) -> Option<[u8; 4]> {
    let parts: Vec<_> = address.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    Some([
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
        parts[3].parse().ok()?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_ipv4_and_ipv6() {
        assert_eq!(
            core_parse_lan_endpoint("192.168.1.7:9999".into(), 45892),
            Some(CoreLanEndpoint {
                host: "192.168.1.7".into(),
                port: 9999
            })
        );
        assert_eq!(
            core_parse_lan_endpoint("[fe80::1]:123".into(), 45892),
            Some(CoreLanEndpoint {
                host: "fe80::1".into(),
                port: 123
            })
        );
        assert_eq!(
            core_parse_lan_endpoint("host.local".into(), 45892)
                .unwrap()
                .port,
            45892
        );
        assert!(core_parse_lan_endpoint("host:0".into(), 45892).is_none());
    }

    #[test]
    fn endpoint_link_round_trips() {
        let endpoint = CoreLanEndpoint {
            host: "fe80::cafe".into(),
            port: 45_892,
        };
        let link = core_make_lan_endpoint_link(endpoint.clone());
        let fragment = link.split_once('#').unwrap().1.to_string();
        assert_eq!(core_parse_lan_endpoint_link(Some(fragment)), Some(endpoint));
    }

    #[test]
    fn subnet_excludes_self_and_network_edges() {
        let hosts = core_subnet_24_hosts("10.2.3.9".into());
        assert_eq!(hosts.len(), 253);
        assert!(!hosts.contains(&"10.2.3.9".to_string()));
        assert_eq!(hosts.first().unwrap(), "10.2.3.1");
        assert_eq!(hosts.last().unwrap(), "10.2.3.254");
    }

    #[test]
    fn network_id_is_stable_within_subnet() {
        assert_eq!(
            core_lan_network_id_for_ipv4("192.168.8.1".into()),
            core_lan_network_id_for_ipv4("192.168.8.200".into())
        );
        assert_ne!(
            core_lan_network_id_for_ipv4("192.168.8.1".into()),
            core_lan_network_id_for_ipv4("192.168.9.1".into())
        );
        assert_eq!(
            core_lan_network_id_for_ipv4("10.154.189.58".into()).as_deref(),
            Some("NcJ68sf-sL-VO63PUTnngg==")
        );
    }

    #[test]
    fn cache_and_resend_policies_handle_time() {
        assert!(lan_endpoint_cache_is_fresh(1_000, 2_000));
        assert!(!lan_endpoint_cache_is_fresh(
            0,
            LAN_ENDPOINT_CACHE_MAX_AGE_MS + 1
        ));
        assert!(!should_resend_lan_endpoint(
            Some("same".into()),
            Some(1_000),
            "same".into(),
            2_000
        ));
        assert!(should_resend_lan_endpoint(
            Some("old".into()),
            Some(1_000),
            "new".into(),
            2_000
        ));
    }
}
