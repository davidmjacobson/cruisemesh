use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Result;
use cruisemesh_core::{core_subnet_24_hosts, LAN_DEFAULT_TCP_PORT};
use tokio::sync::Semaphore;

use crate::lan::transport::LanTransport;

const MAX_CONCURRENT_PROBES: usize = 8;
const PROBE_TIMEOUT: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepResult {
    pub attempted: u16,
    pub authenticated: u16,
}

pub async fn sweep_ipv4(local_ipv4: &str, transport: Arc<LanTransport>) -> Result<SweepResult> {
    let hosts = core_subnet_24_hosts(local_ipv4.to_string());
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES));
    let mut tasks = Vec::with_capacity(hosts.len());
    for host in hosts {
        let Ok(address) = format!("{host}:{LAN_DEFAULT_TCP_PORT}").parse::<SocketAddr>() else {
            continue;
        };
        let permit = semaphore.clone().acquire_owned().await?;
        let transport = transport.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            tokio::time::timeout(
                PROBE_TIMEOUT,
                transport.connect_authenticated(address, None),
            )
            .await
            .is_ok_and(|result| result.is_ok())
        }));
    }
    let mut result = SweepResult {
        attempted: tasks.len().min(u16::MAX as usize) as u16,
        authenticated: 0,
    };
    for task in tasks {
        if task.await.unwrap_or(false) {
            result.authenticated = result.authenticated.saturating_add(1);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_is_bounded_to_one_slash_24() {
        assert_eq!(core_subnet_24_hosts("192.168.50.7".into()).len(), 253);
        assert!(core_subnet_24_hosts("not-an-ip".into()).is_empty());
    }
}
