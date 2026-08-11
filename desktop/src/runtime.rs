use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{atomic::AtomicU16, Arc},
    time::Duration,
};

use anyhow::{Context, Result};
use cruisemesh_core::{
    core_lan_network_id_for_ipv4, encode_lan_endpoint_content, CoreRelayEndpointConfig, Identity,
    LanEndpointContent, MessageStore, KIND_LAN_ENDPOINT_HINT,
};
use rand::{thread_rng, RngCore};

use crate::{
    bootstrap::BootstrapStore,
    ipc,
    lan::{
        endpoint_cache::EndpointCache,
        mdns::{eligible_ipv4_addresses, MdnsDiscovery},
        session::{PeerHub, SessionServices},
        sweep::sweep_ipv4,
        transport::LanTransport,
    },
    mesh::inbound::InboundExecutor,
    platform::{
        lifecycle::{SingleInstance, SleepGuard},
        tray::TrayIcon,
    },
    relay::{
        executor::{build_relay_plan, RelayScheduleState},
        run_relay_pass, RelayHttpClient,
    },
    store_paths::AppPaths,
};

const RELAY_POLL: Duration = Duration::from_secs(60);

pub async fn run(paths: AppPaths, bootstrap: Arc<BootstrapStore>) -> Result<()> {
    let _instance = SingleInstance::acquire()?;
    let _sleep = SleepGuard::prevent_system_sleep(bootstrap.config().prevent_sleep_on_ac)?;
    let (_tray, tray_quit) = TrayIcon::start()?;
    std::fs::write(&paths.ipc_lock, std::process::id().to_string())
        .context("failed to write diagnostic IPC lock")?;

    let identity = bootstrap.identity().clone();
    let store = bootstrap.store();
    let endpoints = EndpointCache::new(paths.endpoint_cache.clone());
    let inbound = InboundExecutor::start(store.clone(), identity.clone(), endpoints.clone())?;
    let hub = Arc::new(PeerHub::new(&identity));
    let mut token = vec![0_u8; 8];
    thread_rng().fill_bytes(&mut token);
    let listen_port = Arc::new(AtomicU16::new(0));
    let relay_nudge = Arc::new(tokio::sync::Notify::new());
    let services = SessionServices {
        identity: identity.clone(),
        store: store.clone(),
        inbound: inbound.clone(),
        hub: hub.clone(),
        endpoints: endpoints.clone(),
        instance_token: token.clone(),
        listen_port: listen_port.clone(),
        relay_nudge: relay_nudge.clone(),
    };
    let transport = Arc::new(LanTransport::bind(services).await?);
    let listening_port = transport.port()?;
    listen_port.store(listening_port, std::sync::atomic::Ordering::Relaxed);
    let mut discovery = Some(MdnsDiscovery::start(transport.clone(), &token)?);
    tracing::info!(listening_port, "CruiseMesh Helper LAN listener started");

    let server = tokio::spawn(transport.clone().serve());
    let ipc_server = tokio::spawn(ipc::serve(
        bootstrap.clone(),
        hub.clone(),
        relay_nudge.clone(),
    ));
    let hint_task = tokio::spawn(lan_hint_loop(
        store.clone(),
        identity.clone(),
        inbound.clone(),
        hub.clone(),
        token.clone(),
        listening_port,
        relay_nudge.clone(),
    ));
    let firewall_hub = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        if firewall_hub.connected_peer_count() == 0 {
            tracing::warn!(
                "No LAN peers have connected; Windows Firewall may be blocking inbound Wi-Fi. Run `cruisemesh-node allow-firewall` from an elevated prompt after approving Public-network access."
            );
        }
    });
    let cached_connector = tokio::spawn(cached_endpoint_loop(
        store.clone(),
        endpoints,
        transport.clone(),
    ));
    spawn_sweeps(transport.clone(), "initial");

    let relay = relay_loop(bootstrap.clone(), inbound.clone(), relay_nudge.clone());
    tokio::pin!(relay);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    let mut last_heartbeat = tokio::time::Instant::now();
    tokio::select! {
        result = &mut relay => result?,
        result = server => result.context("LAN listener task stopped")??,
        result = hint_task => result.context("LAN endpoint hint task stopped")??,
        result = ipc_server => result.context("IPC server task stopped")??,
        result = cached_connector => result.context("cached endpoint task stopped")??,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutdown requested"),
        _ = tray_quit => tracing::info!("tray requested shutdown"),
        result = async {
            loop {
                heartbeat.tick().await;
                let now = tokio::time::Instant::now();
                if now.duration_since(last_heartbeat) > Duration::from_secs(90) {
                    tracing::info!("system resume detected; restarting LAN discovery and relay");
                    drop(discovery.take());
                    discovery = Some(MdnsDiscovery::start(transport.clone(), &token)?);
                    spawn_sweeps(transport.clone(), "resume");
                    relay_nudge.notify_one();
                }
                last_heartbeat = now;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        } => result?,
    }
    drop(discovery);
    let _ = std::fs::remove_file(&paths.ipc_lock);
    Ok(())
}

async fn cached_endpoint_loop(
    store: Arc<MessageStore>,
    endpoints: EndpointCache,
    transport: Arc<LanTransport>,
) -> Result<()> {
    loop {
        let now = now_ms();
        let local_networks: HashSet<Vec<u8>> = eligible_ipv4_addresses()
            .unwrap_or_default()
            .into_iter()
            .filter_map(core_lan_network_id_for_ipv4)
            .map(String::into_bytes)
            .collect();
        for contact in store.list_contacts()? {
            for endpoint in endpoints.fresh_for_contact(&contact.user_id, now)? {
                if !local_networks.contains(&endpoint.network_id) {
                    continue;
                }
                let Ok(address) =
                    format!("{}:{}", endpoint.host, endpoint.port).parse::<SocketAddr>()
                else {
                    continue;
                };
                let transport = transport.clone();
                let expected = contact.user_id.clone();
                tokio::spawn(async move {
                    if let Err(error) = transport
                        .connect_authenticated(address, Some(expected))
                        .await
                    {
                        tracing::debug!(%error, "cached LAN endpoint did not authenticate");
                    }
                });
            }
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn lan_hint_loop(
    store: Arc<MessageStore>,
    identity: Identity,
    inbound: InboundExecutor,
    hub: Arc<PeerHub>,
    instance_token: Vec<u8>,
    port: u16,
    relay_nudge: Arc<tokio::sync::Notify>,
) -> Result<()> {
    loop {
        let now = now_ms();
        let contacts = store.list_contacts()?;
        for host in eligible_ipv4_addresses().unwrap_or_default() {
            let Some(network_id) = core_lan_network_id_for_ipv4(host.clone()) else {
                continue;
            };
            let payload = encode_lan_endpoint_content(LanEndpointContent {
                instance_token: instance_token.clone(),
                network_id: network_id.into_bytes(),
                host,
                port,
                expires_at_ms: now.saturating_add(15 * 60 * 1_000),
            })?;
            for contact in &contacts {
                let authored = store.author_pairwise_message(
                    identity.clone(),
                    contact.clone(),
                    KIND_LAN_ENDPOINT_HINT,
                    payload.clone(),
                    None,
                    now,
                )?;
                inbound.record_authored(authored.envelope.msg_id);
                let _ = hub.send_to_peer(&contact.user_id, authored.frame).await;
                relay_nudge.notify_one();
            }
        }
        tokio::time::sleep(Duration::from_secs(10 * 60)).await;
    }
}

fn spawn_sweeps(transport: Arc<LanTransport>, reason: &'static str) {
    for local in eligible_ipv4_addresses().unwrap_or_default() {
        let transport = transport.clone();
        tokio::spawn(async move {
            match sweep_ipv4(&local, transport).await {
                Ok(result) => tracing::info!(
                    reason,
                    attempted = result.attempted,
                    authenticated = result.authenticated,
                    "LAN fallback sweep finished"
                ),
                Err(error) => tracing::debug!(%error, "LAN fallback sweep failed"),
            }
        });
    }
}

async fn relay_loop(
    bootstrap: Arc<BootstrapStore>,
    inbound: InboundExecutor,
    nudge: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let http = RelayHttpClient::new()?;
    let mut schedule = RelayScheduleState::default();
    loop {
        let own = bootstrap
            .relay_config()?
            .map(|relay| CoreRelayEndpointConfig {
                url: relay.relay_url,
                token: relay.member_token,
            });
        let plan = build_relay_plan(
            &bootstrap.store(),
            bootstrap.identity(),
            own,
            bootstrap.config().share_online,
            schedule.swept_this_session,
            schedule.consecutive_rate_limits,
            schedule.quiet_until_ms,
        )?;
        let result = run_relay_pass(bootstrap.store(), plan, &http, "wn").await?;
        let locally_consumed = inbound
            .drain_relay_sourced(&bootstrap.store(), now_ms())
            .await?;
        schedule.observe(&result.summary);
        tracing::info!(
            outcome = ?result.summary.outcome,
            requests = result.summary.requests_issued,
            rows_ingested = result.summary.rows_ingested,
            locally_consumed,
            "relay pass finished"
        );
        let now = now_ms();
        let delay = if let Some(until) = result.sleep_until_ms {
            Duration::from_millis(until.saturating_sub(now).max(1) as u64)
        } else if result.summary.continuation.is_some() {
            result
                .summary
                .continuation
                .map(|continuation| {
                    Duration::from_millis(
                        continuation.not_before_ms.saturating_sub(now).max(1) as u64
                    )
                })
                .unwrap_or(Duration::from_secs(1))
        } else {
            RELAY_POLL
        };
        tokio::select! {
            _ = tokio::time::sleep(delay) => {},
            _ = nudge.notified() => {},
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
