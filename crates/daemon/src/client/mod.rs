//! Frontend-owned Session protocol, transport, and typed terminal view.
//!
//! The daemon server owns host Sessions and opaque remote tunnels. This boundary
//! owns frontend attachment epochs, correlation, recovery, and control budgets.

pub mod ipc;
#[cfg(any(unix, windows))]
mod session;
#[cfg(any(unix, windows))]
mod transport;
pub mod view;

#[cfg(any(unix, windows))]
pub use ipc::LocalPairingClient;
pub use ipc::{LocalClient, LocalDeviceClient};
#[cfg(any(unix, windows))]
pub use session::{LocalAttachmentEvent, LocalTakeoverRetryToken, SessionClient};
#[cfg(any(unix, windows))]
pub(crate) use transport::{RemoteDaemonRestarter, UnixAttachmentConnector};

/// Creates a desktop upload connector using the existing opaque daemon tunnel.
#[cfg(any(unix, windows))]
pub fn upload_connector(
    socket: impl Into<std::path::PathBuf>,
) -> std::sync::Arc<dyn voytty_client::upload::UploadConnector> {
    std::sync::Arc::new(transport::UnixAttachmentConnector::new(socket))
}

#[cfg(any(unix, windows))]
pub(super) use voytty_client::protocol::{
    decode_response, malformed, resource_error, service_error,
};

#[cfg(any(unix, windows))]
pub(super) use voytty_client::protocol::DEFAULT_DEADLINE;

#[cfg(any(unix, windows))]
use transport::{connect_error, daemon_io};
