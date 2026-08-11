use std::{collections::HashMap, net::IpAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use cruisemesh_core::{lan_endpoint_host_is_local, LAN_SERVICE_TYPE};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rand::{thread_rng, RngCore};

use crate::lan::transport::LanTransport;

const PROTOCOL_VERSION: &str = "1";
const LOSER_FALLBACK: Duration = Duration::from_secs(15);

pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
    service_fullname: String,
    token: String,
}

impl MdnsDiscovery {
    pub fn start(transport: Arc<LanTransport>, instance_token: &[u8]) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("failed to start mDNS daemon")?;
        let service_type = service_type();
        let token = hex(instance_token);
        let instance = format!("cm-{}", random_hex(8));
        let hostname = format!("{}.local.", instance);
        let addresses = eligible_addresses()?;
        let properties = HashMap::from([
            ("v".to_string(), PROTOCOL_VERSION.to_string()),
            ("t".to_string(), token.clone()),
        ]);
        let info = ServiceInfo::new(
            &service_type,
            &instance,
            &hostname,
            addresses.as_slice(),
            transport.port()?,
            properties,
        )?
        .enable_addr_auto();
        let service_fullname = info.get_fullname().to_string();
        daemon.register(info)?;

        let receiver = daemon.browse(&service_type)?;
        let local_token = token.clone();
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv_async().await {
                let ServiceEvent::ServiceResolved(info) = event else {
                    continue;
                };
                let Some(remote_token) = info.get_property_val_str("t") else {
                    continue;
                };
                if remote_token == local_token
                    || info.get_property_val_str("v") != Some(PROTOCOL_VERSION)
                {
                    continue;
                }
                let delay = if local_token.as_str() < remote_token {
                    Duration::ZERO
                } else {
                    LOSER_FALLBACK
                };
                let endpoints: Vec<_> = info
                    .get_addresses()
                    .iter()
                    .filter(|address| lan_endpoint_host_is_local(address.to_string()))
                    .map(|address| std::net::SocketAddr::new(address.to_ip_addr(), info.get_port()))
                    .collect();
                for endpoint in endpoints {
                    let transport = transport.clone();
                    tokio::spawn(async move {
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        if let Err(error) = transport.connect(endpoint, None).await {
                            tracing::debug!(%error, "mDNS LAN connection ended");
                        }
                    });
                }
            }
        });

        Ok(Self {
            daemon,
            service_fullname,
            token,
        })
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.service_fullname);
        let _ = self.daemon.shutdown();
    }
}

fn service_type() -> String {
    format!("{LAN_SERVICE_TYPE}local.")
}

fn eligible_addresses() -> Result<Vec<IpAddr>> {
    let addresses: Vec<_> = if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|interface| {
            let name = interface.name.to_ascii_lowercase();
            !interface.is_loopback()
                && !["vpn", "tunnel", "tap", "tun", "loopback"]
                    .iter()
                    .any(|needle| name.contains(needle))
        })
        .map(|interface| interface.ip())
        .filter(|address| lan_endpoint_host_is_local(address.to_string()))
        .collect();
    if addresses.is_empty() {
        anyhow::bail!("no eligible local Ethernet or Wi-Fi address is available");
    }
    Ok(addresses)
}

pub fn eligible_ipv4_addresses() -> Result<Vec<String>> {
    Ok(eligible_addresses()?
        .into_iter()
        .filter_map(|address| match address {
            IpAddr::V4(address) => Some(address.to_string()),
            IpAddr::V6(_) => None,
        })
        .collect())
}

fn random_hex(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    thread_rng().fill_bytes(&mut value);
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_type_is_dns_sd_qualified_without_identity() {
        assert_eq!(service_type(), "_cruisemesh._tcp.local.");
        let token = random_hex(16);
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|value| value.is_ascii_hexdigit()));
    }
}
