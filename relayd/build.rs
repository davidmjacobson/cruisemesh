use std::env;
use std::process::Command;

/// FR4: embed a build-time git SHA into the binary so the live relay's
/// exact commit is queryable (`/healthz`, startup log) instead of "whatever
/// tree happened to be checked out when someone last ran
/// `docker compose up --build`".
///
/// Two paths, in priority order:
/// 1. A `GIT_SHA` env var, so a Docker build -- whose context does not
///    include `.git` (see `relayd/Dockerfile`) -- can inject it via
///    `--build-arg GIT_SHA=$(git rev-parse --short HEAD)`.
/// 2. `git rev-parse --short HEAD` run directly against the source tree
///    (covers `cargo build`/`cargo test` from a normal checkout or
///    worktree, where `.git` is present).
///
/// Falls back to "unknown" if neither is available, rather than failing the
/// build -- a missing version string is a minor inconvenience, not worth
/// breaking `cargo test` on a machine without git on PATH.
fn main() {
    let git_sha = env::var("GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=CRUISEMESH_GIT_SHA={git_sha}");
    println!("cargo:rerun-if-env-changed=GIT_SHA");
}
