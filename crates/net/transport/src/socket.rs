//! UDP socket construction with enlarged OS-level buffers for QUIC.

use std::io;
use std::net::SocketAddr;

use socket2::{Domain, Protocol, Socket, Type};

/// Desired UDP socket buffer size (7 MiB), matching quic-go's target.
///
/// The OS caps what it actually grants based on `net.core.rmem_max` (Linux) or
/// `kern.ipc.maxsockbuf` (macOS). Setting it here — before handing the socket
/// to Quinn — prevents Quinn from attempting its own resize and emitting a
/// warning when the system limit is lower than requested.
const SOCKET_BUFFER_SIZE: usize = 7 * 1024 * 1024;

/// Creates a bound UDP socket with enlarged send/receive buffers.
///
/// Buffer resizing is best-effort: errors are silently ignored so the socket
/// is always returned even when the OS caps the size. Quinn takes ownership
/// and sets the socket to non-blocking mode internally.
pub(crate) fn build_udp_socket(addr: SocketAddr) -> io::Result<std::net::UdpSocket> {
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    // Best-effort: silently accept whatever the OS grants.
    let _ = socket.set_recv_buffer_size(SOCKET_BUFFER_SIZE);
    let _ = socket.set_send_buffer_size(SOCKET_BUFFER_SIZE);
    socket.bind(&addr.into())?;
    Ok(socket.into())
}
