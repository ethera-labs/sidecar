//! UDP socket construction with OS-level buffer tuning for QUIC.

use std::io;
use std::net::SocketAddr;

use socket2::{Domain, Protocol, Socket, Type};

/// Target `SO_RCVBUF` / `SO_SNDBUF` size (7 MiB).
///
/// Applied before the socket is handed to Quinn so that Quinn does not attempt
/// its own resize. The OS may grant less depending on system limits
/// (`net.core.rmem_max` on Linux, `kern.ipc.maxsockbuf` on macOS).
const SOCKET_BUFFER_SIZE: usize = 7 * 1024 * 1024;

/// Creates a UDP socket bound to `addr` with enlarged send and receive buffers.
///
/// Buffer resizing is best-effort; if the OS rejects the requested size the
/// socket is still returned with whatever buffers the OS allocated.
pub(crate) fn build_udp_socket(addr: SocketAddr) -> io::Result<std::net::UdpSocket> {
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_recv_buffer_size(SOCKET_BUFFER_SIZE);
    let _ = socket.set_send_buffer_size(SOCKET_BUFFER_SIZE);
    socket.bind(&addr.into())?;
    Ok(socket.into())
}
