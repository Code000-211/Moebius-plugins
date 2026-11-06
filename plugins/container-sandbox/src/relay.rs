//! The relay that gives a Docker run the router's ports, where Podman has pasta (see [`crate::container`]).
//!
//! A Docker container cannot reach the host's loopback, and its network in mode `moebius` is none at all, so the ports
//! are carried over Unix sockets in a directory made for the run and mounted in the container. On the host,
//! `container-sandbox run` listens on `<dir>/<port>` for each port ([`outside`]) and connects what arrives to that
//! port of the host's loopback. In the container, this program, started as `container-sandbox relay`, listens on each
//! port of the container's loopback ([`inside`]) and connects what arrives to the socket of that port, then runs the
//! harness. Only those ports are relayed, so the rest of the host stays out of reach.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Why a side of the relay cannot listen; the run then fails, since without the ports the harness cannot work.
#[derive(Debug, thiserror::Error)]
#[error("the relay of the router's ports cannot listen on {at}: {why}")]
pub struct RelayError {
    pub at: String,
    pub why: io::Error,
}

/// The host's side: listens on `<dir>/<port>` for each port, and connects each connection to that port of the host's
/// loopback, until this process ends.
pub fn outside(dir: &Path, ports: &[u16]) -> Result<(), RelayError> {
    let listeners = ports
        .iter()
        .map(|port| {
            let path = dir.join(port.to_string());
            UnixListener::bind(&path)
                .map(|listener| (listener, *port))
                .map_err(|why| RelayError {
                    at: path.display().to_string(),
                    why,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (listener, port) in listeners {
        std::thread::spawn(move || serve_outside(listener, port));
    }
    Ok(())
}

/// The container's side: listens on each port of the loopback, on `127.0.0.1` and on `::1` where the container has
/// it, and connects each connection to `<dir>/<port>`, until this process ends.
pub fn inside(dir: &Path, ports: &[u16]) -> Result<(), RelayError> {
    let mut listeners = Vec::new();
    for port in ports {
        let socket = dir.join(port.to_string());
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, *port));
        let listener = TcpListener::bind(v4).map_err(|why| RelayError {
            at: v4.to_string(),
            why,
        })?;
        listeners.push((listener, socket.clone()));
        // A container without IPv6 has no ::1; the harness then reaches the port on 127.0.0.1 alone.
        if let Ok(listener) = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, *port))) {
            listeners.push((listener, socket));
        }
    }
    for (listener, socket) in listeners {
        std::thread::spawn(move || serve_inside(listener, socket));
    }
    Ok(())
}

/// Connects each connection to `listener` to the port `port` of the host's loopback.
fn serve_outside(listener: UnixListener, port: u16) {
    let addresses = [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ];
    for stream in listener.incoming().flatten() {
        // A port nobody listens on is a connection that ends at once, as it would be without the relay.
        if let Ok(target) = TcpStream::connect(&addresses[..]) {
            join(stream, target);
        }
    }
}

/// Connects each connection to `listener` to the Unix socket `socket`.
fn serve_inside(listener: TcpListener, socket: PathBuf) {
    for stream in listener.incoming().flatten() {
        if let Ok(target) = UnixStream::connect(&socket) {
            join(stream, target);
        }
    }
}

/// A connection the relay copies to and from: a TCP one or a Unix socket's.
trait Stream: Read + Write + Send + Sized + 'static {
    fn another(&self) -> io::Result<Self>;
    fn end_writing(&self);
}

impl Stream for TcpStream {
    fn another(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn end_writing(&self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

impl Stream for UnixStream {
    fn another(&self) -> io::Result<Self> {
        self.try_clone()
    }

    fn end_writing(&self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

/// Copies each way between `a` and `b`, each in a thread; the end of one way is passed on as the end of writing, so
/// a request that ends its sending still gets its answer.
fn join(a: impl Stream, b: impl Stream) {
    let (Ok(a_writer), Ok(b_writer)) = (a.another(), b.another()) else {
        return;
    };
    std::thread::spawn(move || copy(a, b_writer));
    std::thread::spawn(move || copy(b, a_writer));
}

fn copy(mut from: impl Stream, mut to: impl Stream) {
    let _ = io::copy(&mut from, &mut to);
    to.end_writing();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server on the host's loopback that answers each connection with what it read, once it has read all of it.
    fn echo_server() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut read = Vec::new();
                stream.read_to_end(&mut read).unwrap();
                stream.write_all(b"echo:").unwrap();
                stream.write_all(&read).unwrap();
            }
        });
        port
    }

    fn ask(stream: &mut impl Stream, text: &[u8]) -> Vec<u8> {
        stream.write_all(text).unwrap();
        stream.end_writing();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        answer
    }

    #[test]
    fn a_connection_to_the_socket_of_a_port_reaches_that_port_of_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let port = echo_server();
        outside(dir.path(), &[port]).unwrap();
        let mut stream = UnixStream::connect(dir.path().join(port.to_string())).unwrap();
        assert_eq!(ask(&mut stream, b"hello"), b"echo:hello");
        // Each connection is its own.
        let mut again = UnixStream::connect(dir.path().join(port.to_string())).unwrap();
        assert_eq!(ask(&mut again, b"again"), b"echo:again");
    }

    #[test]
    fn both_sides_together_carry_a_tcp_connection_to_the_hosts_port() {
        let dir = tempfile::tempdir().unwrap();
        let port = echo_server();
        outside(dir.path(), &[port]).unwrap();
        // The container's side listens on the router's port of its own loopback; here, on the same machine, that port
        // is the server's, so it listens on another one and connects to the same socket.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let inner = listener.local_addr().unwrap().port();
        let socket = dir.path().join(port.to_string());
        std::thread::spawn(move || serve_inside(listener, socket));
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, inner)).unwrap();
        assert_eq!(ask(&mut stream, b"through"), b"echo:through");
    }

    #[test]
    fn a_socket_that_cannot_be_made_is_an_error_naming_it() {
        let error = outside(Path::new("/does/not/exist"), &[7172]).unwrap_err();
        assert!(
            error.to_string().contains("/does/not/exist/7172"),
            "{error}"
        );
    }
}
