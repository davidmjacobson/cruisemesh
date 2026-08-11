use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        SECURITY_ATTRIBUTES,
    },
};

use crate::{bootstrap::BootstrapStore, lan::session::PeerHub};

pub const PIPE_NAME: &str = r"\\.\pipe\CruiseMeshNode";

pub async fn try_request(request: serde_json::Value) -> Result<Option<serde_json::Value>> {
    let client = match ClientOptions::new().open(PIPE_NAME) {
        Ok(client) => client,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::WouldBlock
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let (reader, mut writer) = tokio::io::split(client);
    writer.write_all(&serde_json::to_vec(&request)?).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    let mut lines = BufReader::new(reader).lines();
    let line = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .context("CruiseMesh IPC response timed out")??
        .context("CruiseMesh IPC server closed without a response")?;
    let response: serde_json::Value = serde_json::from_str(&line)?;
    if response.get("type").and_then(|value| value.as_str()) == Some("Error") {
        anyhow::bail!(
            "{}",
            response
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("CruiseMesh IPC request failed")
        );
    }
    Ok(response.get("value").cloned())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
enum Request {
    GetStatus,
    GetFriendCard,
    ImportFriendCard { text: String },
    ImportRelaySetup { text: String },
    SubscribeEvents,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum Response<T> {
    Ok { value: T },
    Error { message: String },
}

#[derive(Debug, Serialize)]
struct NodeStatus {
    #[serde(flatten)]
    bootstrap: crate::bootstrap::BootstrapStatus,
    lan_peers: usize,
}

pub async fn serve(
    bootstrap: Arc<BootstrapStore>,
    hub: Arc<PeerHub>,
    relay_nudge: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let mut server = create_server(true)?;
    loop {
        server.connect().await?;
        let connected = server;
        server = create_server(false)?;
        let bootstrap = bootstrap.clone();
        let hub = hub.clone();
        let relay_nudge = relay_nudge.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(connected, bootstrap, hub, relay_nudge).await {
                tracing::debug!(%error, "named-pipe client disconnected");
            }
        });
    }
}

async fn handle(
    server: NamedPipeServer,
    bootstrap: Arc<BootstrapStore>,
    hub: Arc<PeerHub>,
    relay_nudge: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let (reader, mut writer) = tokio::io::split(server);
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let request: Request = match parse_request(&line) {
            Ok(request) => request,
            Err(error) => {
                write_json(
                    &mut writer,
                    &Response::<()>::Error {
                        message: format!("invalid request: {error}"),
                    },
                )
                .await?;
                continue;
            }
        };
        if matches!(request, Request::SubscribeEvents) {
            loop {
                write_json(
                    &mut writer,
                    &Response::Ok {
                        value: status(&bootstrap, &hub)?,
                    },
                )
                .await?;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
        let response = dispatch(request, &bootstrap, &hub, &relay_nudge);
        write_json(&mut writer, &response).await?;
    }
    Ok(())
}

fn parse_request(line: &str) -> std::result::Result<Request, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    let object = value.as_object().ok_or_else(|| {
        <serde_json::Error as serde::de::Error>::custom("request must be a JSON object")
    })?;
    let allowed: &[&str] = match object.get("command").and_then(|value| value.as_str()) {
        Some("ImportFriendCard") | Some("ImportRelaySetup") => &["command", "text"],
        Some("GetStatus") | Some("GetFriendCard") | Some("SubscribeEvents") => &["command"],
        _ => &["command"],
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "request contains an unknown field",
        ));
    }
    serde_json::from_value(value)
}

fn dispatch(
    request: Request,
    bootstrap: &BootstrapStore,
    hub: &PeerHub,
    relay_nudge: &tokio::sync::Notify,
) -> Response<serde_json::Value> {
    let result = (|| -> Result<serde_json::Value> {
        Ok(match request {
            Request::GetStatus => serde_json::to_value(status(bootstrap, hub)?)?,
            Request::GetFriendCard => serde_json::json!({ "text": bootstrap.friend_link()? }),
            Request::ImportFriendCard { text } => {
                let contact = bootstrap.import_friend(&text)?;
                relay_nudge.notify_one();
                serde_json::json!({ "name": contact.name })
            }
            Request::ImportRelaySetup { text } => {
                bootstrap.import_relay_setup(&text)?;
                relay_nudge.notify_one();
                serde_json::json!({ "imported": true })
            }
            Request::SubscribeEvents => unreachable!("handled as a stream"),
        })
    })();
    match result {
        Ok(value) => Response::Ok { value },
        Err(error) => Response::Error {
            message: error.to_string(),
        },
    }
}

fn status(bootstrap: &BootstrapStore, hub: &PeerHub) -> Result<NodeStatus> {
    Ok(NodeStatus {
        bootstrap: bootstrap.status()?,
        lan_peers: hub.connected_peer_count(),
    })
}

async fn write_json<W: AsyncWriteExt + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<()> {
    writer.write_all(&serde_json::to_vec(value)?).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn create_server(first: bool) -> Result<NamedPipeServer> {
    // The pipe is local-only and its protected DACL grants access to the
    // security descriptor's owner (the current interactive user) and SYSTEM.
    let descriptor_text = wide("D:P(A;;GA;;;OW)(A;;GA;;;SY)");
    let mut descriptor = ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        anyhow::bail!("failed to construct the named-pipe security descriptor");
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    let result = unsafe {
        options.create_with_security_attributes_raw(
            PIPE_NAME,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
        )
    }
    .context("failed to create the CruiseMesh named pipe");
    unsafe { LocalFree(descriptor) };
    result
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_surface_is_frozen_and_rejects_unknown_fields() {
        assert!(parse_request(r#"{"command":"GetStatus"}"#).is_ok());
        assert!(parse_request(r#"{"command":"GetStatus","relayToken":"secret"}"#).is_err());
    }
}
