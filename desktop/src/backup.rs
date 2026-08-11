use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use cruisemesh_core::{
    backup_max_file_bytes, decode_identity_bytes, encode_identity_bytes,
    inspect_restored_message_store, open_backup, sanitize_restored_message_store_with_options,
    seal_backup, BackupContentOptions, BackupInventory, CoreBackupPayload,
};
use serde::Serialize;

use crate::{bootstrap::BootstrapStore, platform::dpapi, store_paths::AppPaths};

const PENDING_DIR: &str = "restore-pending";
const READY_FILE: &str = "ready";

#[derive(Clone, Debug, Serialize)]
pub struct BackupResult {
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RestorePreview {
    pub created_at_ms: i64,
    pub display_name: Option<String>,
    pub inventory: BackupInventoryView,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackupInventoryView {
    pub contacts: u64,
    pub groups: u64,
    pub messages: u64,
}

pub fn create_backup(
    bootstrap: &Arc<BootstrapStore>,
    destination: &str,
    passphrase: String,
) -> Result<BackupResult> {
    let destination = validate_backup_destination(destination)?;
    if destination.exists() {
        bail!("choose a new backup filename; CruiseMesh will not overwrite an existing file");
    }
    let now = now_ms();
    let snapshot = unique_sibling(&bootstrap.paths().messages, "backup-snapshot", "sqlite");
    let result = (|| {
        bootstrap.store().backup_to_with_options(
            snapshot.to_string_lossy().into_owned(),
            BackupContentOptions::default(),
            now,
        )?;
        let sqlite = read_bounded(&snapshot, backup_max_file_bytes() as usize)?;
        let relay = bootstrap.relay_config()?;
        let bytes = seal_backup(
            passphrase,
            CoreBackupPayload {
                identity: encode_identity_bytes(bootstrap.identity().clone()),
                sqlite,
                src_version_code: 2,
                created_at_ms: now,
                display_name: Some(bootstrap.config().display_name.clone()),
                own_avatar: Vec::new(),
                own_avatar_epoch: 0,
                relay_url: relay.as_ref().map(|value| value.relay_url.clone()),
                relay_token: relay.map(|value| value.member_token),
                share_online: bootstrap.config().share_online,
                friends_of_friends_enabled: false,
            },
            None,
        )?;
        atomic_create(&destination, &bytes)?;
        Ok(BackupResult {
            bytes_written: bytes.len() as u64,
        })
    })();
    let _ = fs::remove_file(&snapshot);
    result
}

pub fn preview_backup(source: &str, passphrase: String) -> Result<RestorePreview> {
    let source = validate_backup_source(source)?;
    let bytes = read_bounded(&source, backup_max_file_bytes() as usize)?;
    let payload = open_backup(passphrase, bytes)?;
    decode_identity_bytes(payload.identity.clone()).context("backup identity is invalid")?;
    let staged = unique_sibling(&source, "restore-preview", "sqlite");
    let result = (|| {
        atomic_create(&staged, &payload.sqlite)?;
        let inventory =
            inspect_restored_message_store(staged.to_string_lossy().into_owned(), now_ms())?;
        Ok(RestorePreview {
            created_at_ms: payload.created_at_ms,
            display_name: payload.display_name,
            inventory: inventory_view(inventory),
        })
    })();
    remove_sqlite_family(&staged);
    result
}

pub fn stage_restore(
    bootstrap: &Arc<BootstrapStore>,
    source: &str,
    passphrase: String,
) -> Result<RestorePreview> {
    let source = validate_backup_source(source)?;
    let bytes = read_bounded(&source, backup_max_file_bytes() as usize)?;
    let payload = open_backup(passphrase, bytes)?;
    let identity =
        decode_identity_bytes(payload.identity.clone()).context("backup identity is invalid")?;
    let root = &bootstrap.paths().root;
    let pending = root.join(PENDING_DIR);
    if pending.exists() {
        bail!("a restore is already staged; restart CruiseMesh before staging another");
    }
    let staging = root.join(format!(
        ".restore-staging-{}-{}",
        std::process::id(),
        now_ms()
    ));
    fs::create_dir(&staging).context("failed to create restore staging directory")?;
    let staged_db = staging.join("messages.db");
    let result = (|| {
        atomic_create(&staged_db, &payload.sqlite)?;
        let inventory =
            inspect_restored_message_store(staged_db.to_string_lossy().into_owned(), now_ms())?;
        sanitize_restored_message_store_with_options(
            staged_db.to_string_lossy().into_owned(),
            BackupContentOptions::default(),
            now_ms(),
        )?;

        atomic_create(
            &staging.join("identity.dpapi"),
            &dpapi::protect(&encode_identity_bytes(identity))?,
        )?;
        let mut config = bootstrap.config().clone();
        if let Some(name) = payload.display_name.clone() {
            config.display_name = name;
        }
        config.share_online = payload.share_online;
        atomic_create(
            &staging.join("config.json"),
            &serde_json::to_vec_pretty(&config)?,
        )?;
        if let (Some(relay_url), Some(member_token)) =
            (payload.relay_url.clone(), payload.relay_token.clone())
        {
            let relay = serde_json::json!({
                "relay_url": relay_url,
                "member_token": member_token,
            });
            atomic_create(
                &staging.join("relay.json.dpapi"),
                &dpapi::protect(&serde_json::to_vec(&relay)?)?,
            )?;
        }
        atomic_create(&staging.join(READY_FILE), b"CMRESTORE1")?;
        fs::rename(&staging, &pending).context("failed to publish staged restore")?;
        Ok(RestorePreview {
            created_at_ms: payload.created_at_ms,
            display_name: payload.display_name,
            inventory: inventory_view(inventory),
        })
    })();
    if result.is_err() {
        remove_tree_if_staging(&staging);
    }
    result
}

/// Install a completely validated restore before identity or SQLite is opened.
/// Existing live files are retained in a timestamped recovery directory.
pub fn apply_pending_restore(paths: &AppPaths) -> Result<bool> {
    let pending = paths.root.join(PENDING_DIR);
    if !pending.join(READY_FILE).is_file() {
        return Ok(false);
    }
    for required in ["identity.dpapi", "messages.db", "config.json"] {
        if !pending.join(required).is_file() {
            bail!("staged restore is incomplete: missing {required}");
        }
    }
    let recovery = paths.root.join(format!("restore-previous-{}", now_ms()));
    fs::create_dir(&recovery).context("failed to create pre-restore recovery directory")?;

    let live = [
        paths.identity.clone(),
        paths.messages.clone(),
        paths.root.join("messages.db-wal"),
        paths.root.join("messages.db-shm"),
        paths.root.join("messages.db-journal"),
        paths.config.clone(),
        paths.relay.clone(),
    ];
    let mut moved = Vec::new();
    for path in live.iter().filter(|path| path.exists()) {
        let destination = recovery.join(path.file_name().context("live path has no filename")?);
        fs::rename(path, &destination)
            .with_context(|| format!("failed to preserve {} before restore", path.display()))?;
        moved.push((path.clone(), destination));
    }

    let installs = [
        (pending.join("identity.dpapi"), paths.identity.clone()),
        (pending.join("messages.db"), paths.messages.clone()),
        (pending.join("config.json"), paths.config.clone()),
        (pending.join("relay.json.dpapi"), paths.relay.clone()),
    ];
    let mut installed: Vec<PathBuf> = Vec::new();
    for (source, destination) in installs {
        if !source.exists() {
            continue;
        }
        if let Err(error) = fs::rename(&source, &destination) {
            for destination in installed.iter().rev() {
                let _ = fs::rename(
                    destination,
                    pending.join(destination.file_name().unwrap_or_default()),
                );
            }
            for (live, saved) in moved.iter().rev() {
                let _ = fs::rename(saved, live);
            }
            return Err(error).context("failed to install staged restore");
        }
        installed.push(destination);
    }
    fs::remove_file(pending.join(READY_FILE)).context("failed to finalize restore marker")?;
    fs::remove_dir(&pending).context("failed to remove empty restore staging directory")?;
    Ok(true)
}

fn inventory_view(value: BackupInventory) -> BackupInventoryView {
    BackupInventoryView {
        contacts: value.contact_count,
        groups: value.group_count,
        messages: value.message_count,
    }
}

fn validate_backup_destination(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value.trim());
    if !path.is_absolute() || !path.parent().is_some_and(Path::is_dir) {
        bail!("backup destination must be an absolute path in an existing folder");
    }
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmbak"))
    {
        bail!("backup filename must end in .cmbak");
    }
    Ok(path)
}

fn validate_backup_source(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value.trim());
    if !path.is_absolute() || !path.is_file() {
        bail!("backup source must be an existing file");
    }
    Ok(path)
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let declared = fs::metadata(path)?.len();
    if declared > limit as u64 {
        bail!("backup data exceeds the CruiseMesh file limit");
    }
    let bytes = fs::read(path)?;
    if bytes.len() > limit {
        bail!("backup data exceeds the CruiseMesh file limit");
    }
    Ok(bytes)
}

fn atomic_create(destination: &Path, bytes: &[u8]) -> Result<()> {
    if !destination.is_absolute() {
        bail!("destination must be absolute");
    }
    let temporary = unique_sibling(destination, "write", "tmp");
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if destination.exists() {
            bail!("destination already exists");
        }
        fs::rename(&temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn unique_sibling(path: &Path, label: &str, extension: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(
        ".cruisemesh-{label}-{}-{}.{}",
        std::process::id(),
        now_ms(),
        extension
    ))
}

fn remove_sqlite_family(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = PathBuf::from(format!("{}{}", path.display(), suffix));
        let _ = fs::remove_file(candidate);
    }
}

fn remove_tree_if_staging(path: &Path) {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".restore-staging-"))
        && path
            .parent()
            .is_some_and(|parent| parent.ends_with("CruiseMesh"))
    {
        let _ = fs::remove_dir_all(path);
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use cruisemesh_core::open_backup;

    #[test]
    fn encrypted_backup_round_trips_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(temp.path().join("CruiseMesh")).unwrap();
        let bootstrap = Arc::new(BootstrapStore::open(paths).unwrap());
        let destination = temp.path().join("cruisemesh.cmbak");
        let result = create_backup(
            &bootstrap,
            destination.to_str().unwrap(),
            "correct horse battery staple".into(),
        )
        .unwrap();
        assert!(result.bytes_written > 0);
        let payload = open_backup(
            "correct horse battery staple".into(),
            fs::read(&destination).unwrap(),
        )
        .unwrap();
        assert_eq!(
            decode_identity_bytes(payload.identity).unwrap().user_id,
            bootstrap.identity().user_id
        );
        assert!(create_backup(
            &bootstrap,
            destination.to_str().unwrap(),
            "correct horse battery staple".into(),
        )
        .is_err());
    }

    #[test]
    fn restore_is_staged_then_swapped_with_recovery_on_reopen() {
        let source_temp = tempfile::tempdir().unwrap();
        let source_paths = AppPaths::under(source_temp.path().join("CruiseMesh")).unwrap();
        let source = Arc::new(BootstrapStore::open(source_paths).unwrap());
        source
            .update_display_name("Migrated laptop".into())
            .unwrap();
        let source_id = source.identity().user_id.clone();
        let backup_path = source_temp.path().join("migration.cmbak");
        create_backup(
            &source,
            backup_path.to_str().unwrap(),
            "correct horse battery staple".into(),
        )
        .unwrap();

        let target_temp = tempfile::tempdir().unwrap();
        let target_paths = AppPaths::under(target_temp.path().join("CruiseMesh")).unwrap();
        let target = Arc::new(BootstrapStore::open(target_paths.clone()).unwrap());
        let old_id = target.identity().user_id.clone();
        assert_ne!(old_id, source_id);
        stage_restore(
            &target,
            backup_path.to_str().unwrap(),
            "correct horse battery staple".into(),
        )
        .unwrap();
        drop(target);

        assert!(apply_pending_restore(&target_paths).unwrap());
        let restored = BootstrapStore::open(target_paths.clone()).unwrap();
        assert_eq!(restored.identity().user_id, source_id);
        assert_eq!(restored.config().display_name, "Migrated laptop");
        let recovery = fs::read_dir(&target_paths.root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("restore-previous-")
            })
            .expect("pre-restore recovery directory");
        assert!(recovery.path().join("identity.dpapi").is_file());
    }
}
