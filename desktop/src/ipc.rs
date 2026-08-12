use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        SECURITY_ATTRIBUTES,
    },
};

use crate::{
    backup,
    bootstrap::BootstrapStore,
    chat::{AttachmentDraft, ChatService, ReactionTarget},
    lan::session::PeerHub,
    mesh::inbound::InboundExecutor,
};

pub const PIPE_NAME: &str = r"\\.\pipe\CruiseMeshNode";
const MAX_REQUEST_BYTES: usize = 512 * 1024;

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
    GetProtocolInfo,
    GetStatus,
    GetFriendCard,
    ImportFriendCard {
        text: String,
    },
    ImportRelaySetup {
        text: String,
    },
    SubscribeEvents,
    GetAppSnapshot,
    GetConversation {
        conversation_id: String,
        #[serde(default)]
        before_timestamp_ms: Option<i64>,
    },
    SendText {
        conversation_id: String,
        text: String,
        reply_to_id: Option<String>,
    },
    SendAttachment {
        conversation_id: String,
        draft: AttachmentDraft,
        reply_to_id: Option<String>,
    },
    React {
        conversation_id: String,
        target: ReactionTarget,
        emoji: String,
    },
    MarkRead {
        conversation_id: String,
    },
    CreateGroup {
        name: String,
        member_ids: Vec<String>,
    },
    CreateBackup {
        path: String,
        passphrase: String,
    },
    PreviewBackup {
        path: String,
        passphrase: String,
    },
    StageRestore {
        path: String,
        passphrase: String,
    },
    SetProfile {
        display_name: String,
    },
    SetPreferences {
        prevent_sleep_on_ac: bool,
        share_online: bool,
        #[serde(default)]
        friends_of_friends: Option<bool>,
    },
    PreviewFriendCard {
        text: String,
    },
    DeleteContact {
        conversation_id: String,
    },
    SetNickname {
        conversation_id: String,
        nickname: Option<String>,
    },
    SetBlocked {
        conversation_id: String,
        blocked: bool,
    },
    SetMuted {
        conversation_id: String,
        muted: bool,
    },
    ReportContact {
        conversation_id: String,
    },
    RenameGroup {
        conversation_id: String,
        name: String,
    },
    AddGroupMembers {
        conversation_id: String,
        member_ids: Vec<String>,
    },
    ShareContact {
        conversation_id: String,
    },
    AcceptPendingShared {
        requester_id: String,
    },
    DismissPendingShared {
        requester_id: String,
        suppress: bool,
    },
    SetProfilePhoto {
        data_base64: String,
    },
    AcceptTerms,
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
    inbound: InboundExecutor,
    relay_nudge: Arc<tokio::sync::Notify>,
    shutdown: Arc<tokio::sync::Notify>,
    listening_port: u16,
) -> Result<()> {
    let chat = ChatService::new(
        bootstrap.clone(),
        hub.clone(),
        inbound,
        relay_nudge.clone(),
        listening_port,
    );
    let mut server = create_server(true)?;
    loop {
        server.connect().await?;
        let connected = server;
        server = create_server(false)?;
        let bootstrap = bootstrap.clone();
        let hub = hub.clone();
        let relay_nudge = relay_nudge.clone();
        let chat = chat.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(connected, bootstrap, hub, relay_nudge, chat, shutdown).await
            {
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
    chat: ChatService,
    shutdown: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let (reader, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(reader);
    loop {
        let mut bytes = Vec::new();
        let read = (&mut reader)
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .await?;
        if read == 0 {
            break;
        }
        if bytes.len() > MAX_REQUEST_BYTES || !bytes.ends_with(b"\n") {
            write_json(
                &mut writer,
                &Response::<()>::Error {
                    message: "request exceeds the CruiseMesh IPC limit".into(),
                },
            )
            .await?;
            break;
        }
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
        let line = std::str::from_utf8(&bytes).context("IPC request is not UTF-8")?;
        let request: Request = match parse_request(line) {
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
        let response = dispatch(request, &bootstrap, &hub, &relay_nudge, &chat, &shutdown).await;
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
        Some("GetStatus")
        | Some("GetProtocolInfo")
        | Some("GetFriendCard")
        | Some("SubscribeEvents")
        | Some("GetAppSnapshot") => &["command"],
        Some("GetConversation") => &["command", "conversation_id", "before_timestamp_ms"],
        Some("MarkRead") | Some("DeleteContact") | Some("ReportContact") | Some("ShareContact") => {
            &["command", "conversation_id"]
        }
        Some("SendText") => &["command", "conversation_id", "text", "reply_to_id"],
        Some("SendAttachment") => &["command", "conversation_id", "draft", "reply_to_id"],
        Some("React") => &["command", "conversation_id", "target", "emoji"],
        Some("CreateGroup") => &["command", "name", "member_ids"],
        Some("AddGroupMembers") => &["command", "conversation_id", "member_ids"],
        Some("RenameGroup") => &["command", "conversation_id", "name"],
        Some("SetNickname") => &["command", "conversation_id", "nickname"],
        Some("SetBlocked") => &["command", "conversation_id", "blocked"],
        Some("SetMuted") => &["command", "conversation_id", "muted"],
        Some("PreviewFriendCard") => &["command", "text"],
        Some("AcceptPendingShared") => &["command", "requester_id"],
        Some("DismissPendingShared") => &["command", "requester_id", "suppress"],
        Some("SetProfilePhoto") => &["command", "data_base64"],
        Some("AcceptTerms") => &["command"],
        Some("CreateBackup") | Some("PreviewBackup") | Some("StageRestore") => {
            &["command", "path", "passphrase"]
        }
        Some("SetProfile") => &["command", "display_name"],
        Some("SetPreferences") => &[
            "command",
            "prevent_sleep_on_ac",
            "share_online",
            "friends_of_friends",
        ],
        _ => &["command"],
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "request contains an unknown field",
        ));
    }
    serde_json::from_value(value)
}

async fn dispatch(
    request: Request,
    bootstrap: &Arc<BootstrapStore>,
    hub: &PeerHub,
    relay_nudge: &tokio::sync::Notify,
    chat: &ChatService,
    shutdown: &Arc<tokio::sync::Notify>,
) -> Response<serde_json::Value> {
    let result: Result<serde_json::Value> = async {
        Ok(match request {
            Request::GetProtocolInfo => serde_json::json!({
                "protocol_version": 4,
                "minimum_ui_version": 3,
            }),
            Request::GetStatus => serde_json::to_value(status(bootstrap, hub)?)?,
            Request::GetFriendCard => serde_json::json!({ "text": bootstrap.friend_link()? }),
            Request::ImportFriendCard { text } => {
                let name = chat.import_and_request_friend(&text).await?;
                serde_json::json!({ "name": name })
            }
            Request::PreviewFriendCard { text } => {
                serde_json::to_value(bootstrap.preview_friend(&text)?)?
            }
            Request::ImportRelaySetup { text } => {
                bootstrap.import_relay_setup(&text)?;
                relay_nudge.notify_one();
                serde_json::json!({ "imported": true })
            }
            Request::SubscribeEvents => unreachable!("handled as a stream"),
            Request::GetAppSnapshot => serde_json::to_value(chat.snapshot()?)?,
            Request::GetConversation {
                conversation_id,
                before_timestamp_ms,
            } => serde_json::to_value(
                chat.conversation_page(&conversation_id, before_timestamp_ms)?,
            )?,
            Request::SendText {
                conversation_id,
                text,
                reply_to_id,
            } => serde_json::to_value(chat.send_text(&conversation_id, text, reply_to_id).await?)?,
            Request::SendAttachment {
                conversation_id,
                draft,
                reply_to_id,
            } => serde_json::to_value(
                chat.send_attachment(&conversation_id, draft, reply_to_id)
                    .await?,
            )?,
            Request::React {
                conversation_id,
                target,
                emoji,
            } => {
                chat.react(&conversation_id, target, emoji).await?;
                serde_json::json!({ "reacted": true })
            }
            Request::MarkRead { conversation_id } => {
                serde_json::json!({ "advanced": chat.mark_read(&conversation_id).await? })
            }
            Request::CreateGroup { name, member_ids } => {
                serde_json::json!({ "conversation_id": chat.create_group(name, member_ids).await? })
            }
            Request::CreateBackup { path, passphrase } => {
                let bootstrap = bootstrap.clone();
                serde_json::to_value(
                    tokio::task::spawn_blocking(move || {
                        backup::create_backup(&bootstrap, &path, passphrase)
                    })
                    .await
                    .context("backup worker stopped")??,
                )?
            }
            Request::PreviewBackup { path, passphrase } => serde_json::to_value(
                tokio::task::spawn_blocking(move || backup::preview_backup(&path, passphrase))
                    .await
                    .context("backup preview worker stopped")??,
            )?,
            Request::StageRestore { path, passphrase } => {
                let bootstrap = bootstrap.clone();
                let preview = tokio::task::spawn_blocking(move || {
                    backup::stage_restore(&bootstrap, &path, passphrase)
                })
                .await
                .context("restore worker stopped")??;
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    shutdown.notify_one();
                });
                serde_json::to_value(preview)?
            }
            Request::SetProfile { display_name } => {
                serde_json::to_value(chat.update_profile(display_name).await?)?
            }
            Request::SetPreferences {
                prevent_sleep_on_ac,
                share_online,
                friends_of_friends,
            } => serde_json::to_value(chat.update_preferences(
                prevent_sleep_on_ac,
                share_online,
                friends_of_friends,
            )?)?,
            Request::DeleteContact { conversation_id } => {
                chat.delete_contact(&conversation_id)?;
                serde_json::json!({ "deleted": true })
            }
            Request::SetNickname {
                conversation_id,
                nickname,
            } => {
                chat.set_nickname(&conversation_id, nickname)?;
                serde_json::json!({ "updated": true })
            }
            Request::SetBlocked {
                conversation_id,
                blocked,
            } => {
                chat.set_blocked(&conversation_id, blocked)?;
                serde_json::json!({ "updated": true })
            }
            Request::SetMuted {
                conversation_id,
                muted,
            } => {
                chat.set_muted(&conversation_id, muted)?;
                serde_json::json!({ "updated": true })
            }
            Request::ReportContact { conversation_id } => {
                serde_json::to_value(chat.report_contact(&conversation_id)?)?
            }
            Request::RenameGroup {
                conversation_id,
                name,
            } => {
                chat.rename_group(&conversation_id, name).await?;
                serde_json::json!({ "updated": true })
            }
            Request::AddGroupMembers {
                conversation_id,
                member_ids,
            } => {
                chat.add_group_members(&conversation_id, member_ids).await?;
                serde_json::json!({ "updated": true })
            }
            Request::ShareContact { conversation_id } => {
                serde_json::to_value(chat.share_contact(&conversation_id)?)?
            }
            Request::AcceptPendingShared { requester_id } => {
                let name = chat.accept_pending_shared(&requester_id).await?;
                serde_json::json!({ "name": name })
            }
            Request::DismissPendingShared {
                requester_id,
                suppress,
            } => {
                chat.dismiss_pending_shared(&requester_id, suppress)?;
                serde_json::json!({ "dismissed": true })
            }
            Request::SetProfilePhoto { data_base64 } => {
                serde_json::to_value(chat.set_profile_photo(data_base64).await?)?
            }
            Request::AcceptTerms => {
                bootstrap.accept_terms()?;
                serde_json::json!({ "accepted": true })
            }
        })
    }
    .await;
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
        assert!(parse_request(r#"{"command":"GetProtocolInfo"}"#).is_ok());
        assert!(parse_request(r#"{"command":"GetStatus"}"#).is_ok());
        assert!(parse_request(r#"{"command":"GetStatus","relayToken":"secret"}"#).is_err());
        assert!(parse_request(
            r#"{"command":"SendText","conversation_id":"person:00","text":"hi","reply_to_id":null}"#
        )
        .is_ok());
        assert!(parse_request(
            r#"{"command":"SendText","conversation_id":"person:00","text":"hi","sealed":"secret"}"#
        )
        .is_err());
        assert!(parse_request(
            r#"{"command":"SetPreferences","prevent_sleep_on_ac":false,"share_online":true}"#
        )
        .is_ok());
        assert!(parse_request(
            r#"{"command":"SetPreferences","prevent_sleep_on_ac":false,"share_online":true,"relay_token":"secret"}"#
        )
        .is_err());
    }
}
