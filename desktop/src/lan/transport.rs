use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use cruisemesh_core::LAN_DEFAULT_TCP_PORT;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{oneshot, Semaphore},
    time::timeout,
};

use crate::lan::session::{run_session, run_session_with_authenticated_signal, SessionServices};

const MAX_CONCURRENT_SESSIONS: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

pub struct LanTransport {
    listener: TcpListener,
    services: SessionServices,
    sessions: Arc<Semaphore>,
}

impl LanTransport {
    pub async fn bind(services: SessionServices) -> Result<Self> {
        let listener = match TcpListener::bind(("0.0.0.0", LAN_DEFAULT_TCP_PORT)).await {
            Ok(listener) => listener,
            Err(_) => TcpListener::bind(("0.0.0.0", 0))
                .await
                .context("failed to bind a LAN listener")?,
        };
        Ok(Self {
            listener,
            services,
            sessions: Arc::new(Semaphore::new(MAX_CONCURRENT_SESSIONS)),
        })
    }

    pub fn port(&self) -> Result<u16> {
        Ok(self.listener.local_addr()?.port())
    }

    pub fn connected_peer_count(&self) -> usize {
        self.services.hub.connected_peer_count()
    }

    pub async fn serve(self: Arc<Self>) -> Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let permit = self.sessions.clone().acquire_owned().await?;
            let services = self.services.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = run_session(stream, false, None, services).await {
                    tracing::debug!(%error, "inbound LAN session ended");
                }
            });
        }
    }

    pub async fn connect(
        &self,
        endpoint: SocketAddr,
        expected_peer: Option<Vec<u8>>,
    ) -> Result<()> {
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(endpoint))
            .await
            .context("LAN connect timed out")??;
        run_session(stream, true, expected_peer, self.services.clone()).await?;
        Ok(())
    }

    /// Starts a durable session but returns as soon as the peer's Noise key has
    /// authenticated as an accepted contact. This is the success condition for
    /// bounded discovery probes; an open TCP socket alone is never sufficient.
    pub async fn connect_authenticated(
        &self,
        endpoint: SocketAddr,
        expected_peer: Option<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(endpoint))
            .await
            .context("LAN connect timed out")??;
        let (authenticated, receive_authenticated) = oneshot::channel();
        let services = self.services.clone();
        tokio::spawn(async move {
            if let Err(error) = run_session_with_authenticated_signal(
                stream,
                true,
                expected_peer,
                services,
                Some(authenticated),
            )
            .await
            {
                tracing::debug!(%error, "outbound LAN session ended");
            }
        });
        receive_authenticated
            .await
            .context("LAN session ended before authentication")
    }
}
