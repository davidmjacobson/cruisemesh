#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
pub mod backup;
#[cfg(windows)]
pub mod bootstrap;
#[cfg(windows)]
pub mod chat;
#[cfg(windows)]
pub mod config;
#[cfg(windows)]
pub mod identity_store;
#[cfg(windows)]
pub mod ipc;
#[cfg(windows)]
pub mod lan;
#[cfg(windows)]
pub mod mesh;
#[cfg(windows)]
pub mod relay;
#[cfg(windows)]
pub mod runtime;
#[cfg(windows)]
pub mod store_paths;

#[cfg(windows)]
pub mod platform;

pub const DEFAULT_DISPLAY_NAME: &str = "Cabin PC";
