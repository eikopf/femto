//! Embedded Tailscale integration and lifecycle management for Femto.

pub mod application_tcp;
pub mod bootstrap;
pub mod control_session;
pub mod control_stream;
pub mod control_worker;
pub mod coordinator;
pub mod derp;
pub mod derp_frames;
pub mod device_config;
pub mod device_enrollment;
pub mod discovery;
pub mod endpoints;
pub mod enrollment;
pub mod handshake;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_bootstrap;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_control;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_reconnect;
#[cfg(all(unix, not(target_os = "espidf")))]
pub use control_worker as host_control_worker;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_derp;
#[cfg(all(unix, not(target_os = "espidf")))]
pub use endpoints as host_endpoints;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_network;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_registration;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_relay_worker;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_runtime;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_tcp;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_tcp_tls;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod host_tls;
pub mod identity_store;
pub mod socket_network;
pub mod socket_udp;
#[cfg(all(unix, not(target_os = "espidf")))]
pub use socket_udp as host_udp;
pub mod map_session;
pub mod network;
pub mod plane;
pub mod policy;
pub mod relay;
pub mod relay_pool;
pub mod relay_worker;
pub mod rotation;
pub mod sealed;
#[cfg(all(unix, not(target_os = "espidf")))]
pub mod store;
pub mod stun;
pub mod tcp;
pub mod timestamp;
pub mod transport;
pub mod udp;
pub mod udp_request;

/// Protocol implementation pinned for reproducible interoperability tests.
/// This export does not imply that every upstream feature is qualified by Femto.
pub use tailscale_esp32 as protocol;
