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
///    `--build-arg GIT_SHA=$(git rev-parse --short HEAD)`. The deploy
///    script `tools/relay_deploy.sh` is the supported way to do that: it
///    reads the value out of the checkout at deploy time, so it cannot go
///    stale the way a value stored in `relayd/.env` could.
/// 2. `git rev-parse --short HEAD` run directly against the source tree
///    (covers `cargo build`/`cargo test` from a normal checkout or
///    worktree, where `.git` is present).
///
/// A plain `cargo build` falls back to "unknown" if neither is available,
/// rather than failing -- a missing version string is a minor
/// inconvenience, not worth breaking `cargo test` on a machine without git
/// on PATH.
///
/// A *deploy* is the opposite case. Setting `CRUISEMESH_REQUIRE_GIT_SHA=1`
/// (the Dockerfile does) turns every route to "unknown" into a hard build
/// failure, because an image that reports the wrong commit is worse than no
/// image: `/healthz` then answers confidently with some earlier deploy's SHA
/// while running new code, which has already sent two deploy investigations
/// down the wrong path.
fn main() {
    let strict = env::var("CRUISEMESH_REQUIRE_GIT_SHA")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);

    let injected = env::var("GIT_SHA").ok().filter(|s| !s.is_empty());

    // "unknown" is never a real answer -- it is the old compose default
    // (`GIT_SHA: ${GIT_SHA:-unknown}`) leaking through. Reject it whether or
    // not this build is strict.
    if let Some(sha) = &injected {
        if sha.eq_ignore_ascii_case("unknown") {
            panic!(
                "GIT_SHA=unknown is a placeholder, not a commit. Build with \
                 `--build-arg GIT_SHA=$(git rev-parse --short HEAD)`, or use \
                 tools/relay_deploy.sh, which does that for you."
            );
        }
        if strict {
            validate_sha(sha);
        }
    } else if strict {
        panic!(
            "CRUISEMESH_REQUIRE_GIT_SHA is set but GIT_SHA is empty, so this \
             image would report the wrong commit on /healthz. Deploy with \
             tools/relay_deploy.sh, or pass \
             `--build-arg GIT_SHA=$(git rev-parse --short HEAD)` explicitly."
        );
    }

    let git_sha = injected
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
    println!("cargo:rerun-if-env-changed=CRUISEMESH_REQUIRE_GIT_SHA");
}

/// Catch a `GIT_SHA` that is not a commit at all -- an unexpanded
/// `$(git rev-parse ...)`, a branch name, a tag -- before it is baked into an
/// image and served from `/healthz` as fact.
///
/// A `-dirty` suffix is allowed and deliberately preserved: that is how
/// `tools/relay_deploy.sh` marks a build taken from a modified tree under its
/// `ALLOW_DIRTY=1` override, so the compromise stays visible in the health
/// response instead of masquerading as the clean commit.
fn validate_sha(sha: &str) {
    let core = sha.strip_suffix("-dirty").unwrap_or(sha);
    let looks_like_a_sha =
        (7..=40).contains(&core.len()) && core.chars().all(|c| c.is_ascii_hexdigit());
    if !looks_like_a_sha {
        panic!(
            "GIT_SHA={sha:?} does not look like a git commit (expected 7-40 hex \
             characters, optionally suffixed `-dirty`). Pass \
             `--build-arg GIT_SHA=$(git rev-parse --short HEAD)`, or use \
             tools/relay_deploy.sh."
        );
    }
}
