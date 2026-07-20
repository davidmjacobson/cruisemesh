use std::env;

use tokio::net::TcpListener;
use tracing::info;

use cruisemesh_relayd::{
    app, parse_bind, parse_family_quota_bytes, parse_tokens, AppState, RelayStore,
    DEFAULT_FAMILY_QUOTA_BYTES,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // FR2: `from_default_env()` silently falls back to ERROR-only when
    // RUST_LOG is unset -- combined with neither the Dockerfile nor
    // docker-compose.yml setting it, the deployed container printed
    // nothing, ever, including this file's own startup line. Default to
    // "info" so a field incident is debuggable out of the box; an operator
    // can still override via RUST_LOG.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let bind = env::var("CRUISEMESH_RELAY_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let db_path =
        env::var("CRUISEMESH_RELAY_DB").unwrap_or_else(|_| "cruisemesh-relayd.sqlite".to_string());
    let tokens = env::var("CRUISEMESH_RELAY_TOKENS").unwrap_or_default();
    let auth_tokens = parse_tokens(&tokens);
    if auth_tokens.is_empty() {
        return Err("set CRUISEMESH_RELAY_TOKENS to a comma-separated allowlist".into());
    }
    let family_quota_bytes = match env::var("CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES") {
        Ok(raw) => parse_family_quota_bytes(&raw)?,
        Err(_) => DEFAULT_FAMILY_QUOTA_BYTES,
    };

    let listener = TcpListener::bind(parse_bind(&bind)?).await?;
    let store = RelayStore::open(&db_path)?;
    info!(
        "relay server listening on {bind}, db={db_path}, family_quota_bytes={family_quota_bytes}"
    );
    axum::serve(
        listener,
        app(AppState::with_family_quota_bytes(
            store,
            auth_tokens,
            family_quota_bytes,
        )),
    )
    .await?;
    Ok(())
}
