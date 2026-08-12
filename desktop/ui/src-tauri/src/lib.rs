#[cfg(not(windows))]
compile_error!("CruiseMesh Desktop is currently supported only on Windows");

#[cfg(windows)]
mod windows_app {
    use std::{
        os::windows::process::CommandExt,
        path::PathBuf,
        process::Command,
        sync::{
            atomic::{AtomicBool, Ordering},
            OnceLock,
        },
        time::Duration,
    };

    use anyhow::{bail, Context, Result};
    use serde_json::{json, Value};
    use tauri::{Emitter, Manager};
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::windows::named_pipe::ClientOptions,
        sync::Mutex,
    };
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    const PIPE_NAME: &str = r"\\.\pipe\CruiseMeshNode";
    const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
    const START_TIMEOUT: Duration = Duration::from_secs(15);
    const UI_PROTOCOL_VERSION: u64 = 3;
    const OLD_HELPER_MESSAGE: &str = "An older CruiseMesh Helper is already running. Right-click the CruiseMesh Helper tray icon, confirm Quit, and leave this window open. It will start the updated helper automatically.";
    static PROTOCOL_READY: AtomicBool = AtomicBool::new(false);

    fn start_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    async fn request(request: Value) -> std::result::Result<Value, String> {
        request_inner(request)
            .await
            .map_err(|error| error.to_string())
    }

    async fn request_inner(request: Value) -> Result<Value> {
        if let Some(value) = try_request(&request).await? {
            return Ok(value);
        }

        let _guard = start_lock().lock().await;
        if let Some(value) = try_request(&request).await? {
            return Ok(value);
        }
        start_node()?;
        let started = tokio::time::Instant::now();
        loop {
            if let Some(value) = try_request(&request).await? {
                return Ok(value);
            }
            if started.elapsed() >= START_TIMEOUT {
                bail!("CruiseMesh Helper did not become ready within 15 seconds");
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn try_request(request: &Value) -> Result<Option<Value>> {
        let client = match ClientOptions::new().open(PIPE_NAME) {
            Ok(client) => client,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::WouldBlock
                ) || error.raw_os_error() == Some(231) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error).context("failed to connect to CruiseMesh Helper"),
        };
        let (reader, mut writer) = tokio::io::split(client);
        writer.write_all(&serde_json::to_vec(request)?).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        let mut reader = BufReader::new(reader);
        let mut bytes = Vec::new();
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            (&mut reader)
                .take((MAX_RESPONSE_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes),
        )
        .await
        .context("CruiseMesh Helper response timed out")??;
        if read == 0 {
            bail!("CruiseMesh Helper closed the request without a response");
        }
        if bytes.len() > MAX_RESPONSE_BYTES || !bytes.ends_with(b"\n") {
            bail!("CruiseMesh Helper response exceeds the desktop limit");
        }
        bytes.pop();
        let response: Value = serde_json::from_slice(&bytes)?;
        if response.get("type").and_then(Value::as_str) == Some("Error") {
            let message = response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("CruiseMesh Helper rejected the request");
            bail!("{}", helper_error_message(message));
        }
        response
            .get("value")
            .cloned()
            .context("CruiseMesh Helper returned an invalid response")
            .map(Some)
    }

    fn helper_error_message(message: &str) -> &str {
        if message.contains("unknown variant `GetProtocolInfo`") {
            OLD_HELPER_MESSAGE
        } else {
            message
        }
    }

    async fn ensure_protocol() -> std::result::Result<(), String> {
        if PROTOCOL_READY.load(Ordering::Acquire) {
            return Ok(());
        }
        let info = request(json!({ "command": "GetProtocolInfo" })).await?;
        let version = info
            .get("protocol_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| "CruiseMesh Helper returned invalid protocol information".to_string())?;
        let minimum_ui = info
            .get("minimum_ui_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| "CruiseMesh Helper returned invalid protocol information".to_string())?;
        if version < UI_PROTOCOL_VERSION || minimum_ui > UI_PROTOCOL_VERSION {
            return Err(format!(
                "This CruiseMesh window uses helper protocol {UI_PROTOCOL_VERSION}, but the running helper requires UI protocol {minimum_ui} and provides protocol {version}. Quit the CruiseMesh Helper from its tray icon, then leave this window open."
            ));
        }
        PROTOCOL_READY.store(true, Ordering::Release);
        Ok(())
    }

    fn start_node() -> Result<()> {
        let executable = node_executable().context(
            "cruisemesh-node.exe was not found beside CruiseMesh; reinstall or set CRUISEMESH_NODE_EXE for development",
        )?;
        Command::new(executable)
            .arg("run")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("failed to start CruiseMesh Helper")?;
        Ok(())
    }

    fn node_executable() -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(explicit) = std::env::var_os("CRUISEMESH_NODE_EXE") {
            candidates.push(PathBuf::from(explicit));
        }
        if let Ok(current) = std::env::current_exe() {
            if let Some(parent) = current.parent() {
                candidates.push(parent.join("cruisemesh-node.exe"));
            }
        }
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("..")
                .join("target")
                .join("debug")
                .join("cruisemesh-node.exe"),
        );
        candidates.into_iter().find(|path| path.is_file())
    }

    #[tauri::command]
    async fn get_app_snapshot() -> std::result::Result<Value, String> {
        ensure_protocol().await?;
        let result = request(json!({ "command": "GetAppSnapshot" })).await;
        if result.is_err() {
            PROTOCOL_READY.store(false, Ordering::Release);
        }
        result
    }

    #[tauri::command]
    async fn get_conversation(conversation_id: String) -> std::result::Result<Value, String> {
        request(json!({
            "command": "GetConversation",
            "conversation_id": conversation_id,
        }))
        .await
    }

    #[tauri::command]
    async fn send_text(
        conversation_id: String,
        text: String,
        reply_to_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        request(json!({
            "command": "SendText",
            "conversation_id": conversation_id,
            "text": text,
            "reply_to_id": reply_to_id,
        }))
        .await
    }

    #[tauri::command]
    async fn send_attachment(
        conversation_id: String,
        draft: Value,
        reply_to_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        request(json!({
            "command": "SendAttachment",
            "conversation_id": conversation_id,
            "draft": draft,
            "reply_to_id": reply_to_id,
        }))
        .await
    }

    #[tauri::command]
    async fn react(
        conversation_id: String,
        target: Value,
        emoji: String,
    ) -> std::result::Result<Value, String> {
        request(json!({
            "command": "React",
            "conversation_id": conversation_id,
            "target": target,
            "emoji": emoji,
        }))
        .await
    }

    #[tauri::command]
    async fn mark_read(conversation_id: String) -> std::result::Result<Value, String> {
        request(json!({
            "command": "MarkRead",
            "conversation_id": conversation_id,
        }))
        .await
    }

    #[tauri::command]
    async fn create_group(
        name: String,
        member_ids: Vec<String>,
    ) -> std::result::Result<Value, String> {
        request(json!({
            "command": "CreateGroup",
            "name": name,
            "member_ids": member_ids,
        }))
        .await
    }

    #[tauri::command]
    async fn import_friend(text: String) -> std::result::Result<Value, String> {
        request(json!({ "command": "ImportFriendCard", "text": text })).await
    }

    #[tauri::command]
    async fn import_relay(text: String) -> std::result::Result<Value, String> {
        request(json!({ "command": "ImportRelaySetup", "text": text })).await
    }

    #[tauri::command]
    async fn create_backup(path: String, passphrase: String) -> std::result::Result<Value, String> {
        request(json!({
            "command": "CreateBackup",
            "path": path,
            "passphrase": passphrase,
        }))
        .await
    }

    #[tauri::command]
    async fn preview_backup(
        path: String,
        passphrase: String,
    ) -> std::result::Result<Value, String> {
        request(json!({
            "command": "PreviewBackup",
            "path": path,
            "passphrase": passphrase,
        }))
        .await
    }

    #[tauri::command]
    async fn stage_restore(path: String, passphrase: String) -> std::result::Result<Value, String> {
        request(json!({
            "command": "StageRestore",
            "path": path,
            "passphrase": passphrase,
        }))
        .await
    }

    #[tauri::command]
    async fn set_profile(display_name: String) -> std::result::Result<Value, String> {
        request(json!({
            "command": "SetProfile",
            "display_name": display_name,
        }))
        .await
    }

    #[tauri::command]
    async fn set_preferences(
        prevent_sleep_on_ac: bool,
        share_online: bool,
    ) -> std::result::Result<Value, String> {
        request(json!({
            "command": "SetPreferences",
            "prevent_sleep_on_ac": prevent_sleep_on_ac,
            "share_online": share_online,
        }))
        .await
    }

    #[tauri::command]
    fn initial_activation() -> Vec<String> {
        std::env::args().collect()
    }

    pub fn run() {
        tauri::Builder::default()
            .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                let _ = app.emit("activation", args);
            }))
            .plugin(tauri_plugin_deep_link::init())
            .plugin(tauri_plugin_dialog::init())
            .plugin(tauri_plugin_notification::init())
            .invoke_handler(tauri::generate_handler![
                get_app_snapshot,
                get_conversation,
                send_text,
                send_attachment,
                react,
                mark_read,
                create_group,
                import_friend,
                import_relay,
                create_backup,
                preview_backup,
                stage_restore,
                set_profile,
                set_preferences,
                initial_activation,
            ])
            .run(tauri::generate_context!())
            .expect("error while running CruiseMesh Desktop");
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn old_helper_protocol_error_becomes_an_upgrade_instruction() {
            assert_eq!(
                helper_error_message(
                    "invalid request: unknown variant `GetProtocolInfo`, expected `GetStatus`"
                ),
                OLD_HELPER_MESSAGE
            );
            assert_eq!(helper_error_message("another failure"), "another failure");
        }
    }
}

#[cfg(windows)]
pub use windows_app::run;
