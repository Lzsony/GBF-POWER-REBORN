#[cfg(not(target_os = "ios"))]
pub mod acceleration;
#[cfg(target_os = "ios")]
#[path = "embedded_ssh.rs"]
pub mod acceleration;
mod acceleration_types;
pub mod authorization;
#[cfg(all(feature = "embedded-ssh", not(target_os = "ios")))]
pub mod embedded_ssh;
#[cfg(target_os = "ios")]
mod ios_security;
pub mod selection;
pub use authorization::EmbeddedPublicProfile;
mod background;
pub mod cache;
pub mod certificate;
pub mod config;
pub mod config_store;
pub mod connection;
pub mod data_directory;
pub mod error;
pub mod metrics;
pub mod preferences;
pub mod probe;
pub mod proxy;
mod proxy_diagnostics;
pub mod routing;
pub mod rules;
pub mod runtime;
mod tcp_probe;

#[cfg(windows)]
mod windows_permissions;

#[cfg(windows)]
pub mod windows_process;

#[cfg(windows)]
pub mod windows_autostart;
