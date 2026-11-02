use super::*;
use socket2::SockRef;
use std::{net::TcpListener, time::Duration};

#[test]
fn gateway_socket_has_bounded_unacknowledged_data_and_idle_keepalive() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let connection = Connection::connect(
        listener.local_addr().unwrap(), 1, Duration::from_secs(2), Duration::from_millis(100),
    ).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    let socket = SockRef::from(connection.stream.as_ref().unwrap());
    assert!(socket.keepalive().unwrap());
    assert_eq!(socket.tcp_keepalive_time().unwrap(), Duration::from_secs(30));
    assert_eq!(socket.tcp_keepalive_interval().unwrap(), Duration::from_secs(5));
    assert_eq!(socket.tcp_keepalive_retries().unwrap(), 3);
    #[cfg(target_os = "linux")]
    assert_eq!(socket.tcp_user_timeout().unwrap(), Some(Duration::from_secs(10)));
}

#[test]
fn quiet_live_tcp_peer_is_idle_without_application_heartbeat_or_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut connection = Connection::connect(
        listener.local_addr().unwrap(), 1, Duration::from_secs(2), Duration::from_millis(100),
    ).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_nonblocking(true).unwrap();
    for _ in 0..4 {
        assert!(matches!(connection.receive().unwrap(), Receive::Idle));
        assert!(connection.is_connected());
    }
    assert_eq!(peer.read(&mut [0; 1]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

struct KernelTimeout;
impl Read for KernelTimeout {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        #[cfg(target_os = "linux")]
        let etimedout = 110;
        #[cfg(target_os = "macos")]
        let etimedout = 60;
        Err(io::Error::from_raw_os_error(etimedout))
    }
}
impl Write for KernelTimeout {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { Ok(bytes.len()) }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

#[test]
fn terminal_kernel_tcp_timeout_is_not_polling_idle_and_closes_stream() {
    let mut connection = Connection::new(KernelTimeout, 9);
    let error = connection.receive().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!connection.is_connected());
    assert!(matches!(connection.receive().unwrap(), Receive::Disconnected { .. }));
}
