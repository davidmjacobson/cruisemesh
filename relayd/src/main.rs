use std::env;

use tokio::net::TcpListener;
use tracing::info;

use cruisemesh_relayd::{app, parse_bind, parse_tokens, AppState, RelayStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let bind = env::var("CRUISEMESH_RELAY_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let db_path = env::var("CRUISEMESH_RELAY_DB").unwrap_or_else(|_| "cruisemesh-relayd.sqlite".to_string());
    let tokens = env::var("CRUISEMESH_RELAY_TOKENS").unwrap_or_default();
    let auth_tokens = parse_tokens(&tokens);
    if auth_tokens.is_empty() {
        return Err("set CRUISEMESH_RELAY_TOKENS to a comma-separated allowlist".into());
    }

    let listener = TcpListener::bind(parse_bind(&bind)?).await?;
    let store = RelayStore::open(&db_path)?;
    info!("relay server listening on {bind}, db={db_path}");
    axum::serve(listener, app(AppState::new(store, auth_tokens))).await?;
    Ok(())
}
