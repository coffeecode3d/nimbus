//! Minimal SOCKS5 proxy for the Windows path.
//!
//! Windows has no per-application routing without a signed kernel driver, so the
//! honest user-mode option is a proxy: applications that understand SOCKS5 send
//! their traffic here, and this proxy dials out **bound to the tunnel adapter's
//! address**, so the connection leaves through Eddie's tunnel.
//!
//! Why binding an address is enough: Windows uses the strong host model (both
//! `weakhostsend` and `weakhostreceive` are disabled by default), so a socket
//! bound to an address that belongs to the tunnel adapter transmits on that
//! adapter. On Linux the same trick does *not* work — routing is by
//! destination — which is why Linux binds the device explicitly instead, and
//! only for testing, since Linux uses the transparent cgroup path.
//!
//! Scope: RFC 1928 CONNECT only, no authentication, loopback only. Chromium
//! resolves names through the proxy for SOCKS5, so DNS stays inside the tunnel.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// How long a client may take to complete the SOCKS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Address to listen on. `None` means `127.0.0.1:0` (an ephemeral port).
    pub listen: Option<SocketAddr>,
    /// Source address for outbound connections: the tunnel's own address.
    pub bind_ip: Option<IpAddr>,
    /// Linux only: bind the outbound socket to this device as well.
    pub bind_device: Option<String>,
}

pub struct Proxy {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Proxy {
    pub fn start(options: Options) -> Result<Self, String> {
        let listen = options.listen.unwrap_or_else(|| {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
        });
        let listener = TcpListener::bind(listen)
            .map_err(|e| format!("could not listen on {listen}: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("could not read the listen address: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("could not set the listener non-blocking: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let options = Arc::new(options);

        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let options = Arc::clone(&options);
                        thread::spawn(move || {
                            if let Err(e) = serve_client(stream, &options) {
                                // A refused or malformed connection is normal:
                                // browsers probe proxies and abort connections.
                                let _ = e;
                            }
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            }
        });

        Ok(Self {
            addr,
            stop,
            worker: Some(worker),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.stop();
        }
    }
}

/// One client: greeting, CONNECT, then a byte pump in both directions.
fn serve_client(mut client: TcpStream, options: &Options) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    client.set_nodelay(true).ok();

    // Greeting: version, method count, methods.
    let mut head = [0u8; 2];
    client.read_exact(&mut head)?;
    if head[0] != 5 {
        return Ok(()); // not SOCKS5; drop quietly
    }
    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods)?;
    // "No authentication required".
    client.write_all(&[5, 0])?;

    // Request: version, command, reserved, address type.
    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    if request[0] != 5 || request[1] != 1 {
        // Only CONNECT is supported. BIND/ASSOCIATE get "command not supported".
        client.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])?;
        return Ok(());
    }

    let (host, port) = match read_target(&mut client, request[3]) {
        Ok(target) => target,
        Err(_) => {
            client.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0])?;
            return Ok(());
        }
    };

    let upstream = match connect_any(&host, port, options) {
        Ok(stream) => stream,
        Err(_) => {
            // 0x05: connection refused.
            client.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])?;
            return Ok(());
        }
    };

    // Success, with the bound address omitted (all zeroes is accepted).
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
    client.set_read_timeout(None).ok();

    pump(client, upstream);
    Ok(())
}

/// Read the destination host and port from a CONNECT request.
fn read_target(client: &mut TcpStream, atyp: u8) -> io::Result<(String, u16)> {
    let host: String = match atyp {
        1 => {
            let mut raw = [0u8; 4];
            client.read_exact(&mut raw)?;
            IpAddr::from(raw).to_string()
        }
        4 => {
            let mut raw = [0u8; 16];
            client.read_exact(&mut raw)?;
            IpAddr::from(raw).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len)?;
            let mut raw = vec![0u8; len[0] as usize];
            client.read_exact(&mut raw)?;
            String::from_utf8(raw).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?
        }
        _ => return Err(io::Error::from(io::ErrorKind::InvalidData)),
    };

    let mut port = [0u8; 2];
    client.read_exact(&mut port)?;
    Ok((host, u16::from_be_bytes(port)))
}

/// Resolve and dial, trying every address the name maps to.
///
/// Resolving here rather than in the client is deliberate: the name is looked up
/// over the tunnel, so it never leaks through the physical connection. Trying
/// each address matters because `localhost` often resolves to `::1` first while
/// the service listens on IPv4 only.
fn connect_any(host: &str, port: u16, options: &Options) -> io::Result<TcpStream> {
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for addr in (host, port).to_socket_addrs()? {
        match connect_bound(&addr, options) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Dial `target` from the tunnel's address so the traffic leaves the tunnel.
///
/// The socket is created by hand because the source address has to be bound
/// *before* connecting, and the local device has to be selected on Linux.
fn connect_bound(target: &SocketAddr, options: &Options) -> io::Result<TcpStream> {
    use socket2::{Domain, Protocol, Socket, Type};

    let domain = match target {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

    #[cfg(target_os = "linux")]
    if let Some(device) = options.bind_device.as_deref() {
        socket.bind_device(Some(device.as_bytes()))?;
    }

    // Binding the source address is what makes Windows send the packet through
    // the adapter that owns it (strong host model).
    if let Some(ip) = options.bind_ip {
        socket.bind(&SocketAddr::new(ip, 0).into())?;
    }

    socket.connect(&(*target).into())?;
    let stream: TcpStream = socket.into();
    stream.set_nodelay(true).ok();
    Ok(stream)
}

/// Copy in both directions until either side closes.
fn pump(client: TcpStream, upstream: TcpStream) {
    let (mut client_read, mut client_write) = match (client.try_clone(), client) {
        (Ok(read), write) => (read, write),
        (Err(_), _) => return,
    };
    let (mut upstream_read, mut upstream_write) = match (upstream.try_clone(), upstream) {
        (Ok(read), write) => (read, write),
        (Err(_), _) => return,
    };

    let forward = thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut upstream_read, &mut client_write);
    let _ = client_write.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

/// The `socks5://` URL an application should be pointed at.
pub fn proxy_url(addr: SocketAddr) -> String {
    format!("socks5://{addr}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A tiny echo server standing in for the far side of the tunnel.
    fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind echo");
        let addr = listener.local_addr().expect("echo addr");
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || {
                    let mut stream = stream;
                    let mut buf = [0u8; 1024];
                    loop {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if stream.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        addr
    }

    fn handshake(proxy: SocketAddr, target: SocketAddr) -> TcpStream {
        let mut client = TcpStream::connect(proxy).expect("connect proxy");
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        client.write_all(&[5, 1, 0]).unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [5, 0], "server must accept no-auth");

        let mut request = vec![5, 1, 0];
        match target.ip() {
            IpAddr::V4(ip) => {
                request.push(1);
                request.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                request.push(4);
                request.extend_from_slice(&ip.octets());
            }
        }
        request.extend_from_slice(&target.port().to_be_bytes());
        client.write_all(&request).unwrap();

        let mut head = [0u8; 4];
        client.read_exact(&mut head).unwrap();
        assert_eq!(head[0], 5);
        assert_eq!(head[1], 0, "CONNECT must succeed");
        let mut rest = [0u8; 6];
        client.read_exact(&mut rest).unwrap();
        client
    }

    #[test]
    fn relays_bytes_over_connect() {
        let echo = echo_server();
        let mut proxy = Proxy::start(Options::default()).expect("start proxy");

        let mut client = handshake(proxy.addr, echo);
        client.write_all(b"hello tunnel").unwrap();
        let mut buf = [0u8; 12];
        client.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello tunnel");

        proxy.stop();
    }

    #[test]
    fn resolves_hostnames_itself() {
        // "localhost" must be resolved by the proxy, not the client.
        let echo = echo_server();
        let mut proxy = Proxy::start(Options::default()).expect("start proxy");

        let mut client = TcpStream::connect(proxy.addr).expect("connect proxy");
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        client.write_all(&[5, 1, 0]).unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).unwrap();

        let name = b"localhost";
        let mut request = vec![5, 1, 0, 3, name.len() as u8];
        request.extend_from_slice(name);
        request.extend_from_slice(&echo.port().to_be_bytes());
        client.write_all(&request).unwrap();

        let mut head = [0u8; 4];
        client.read_exact(&mut head).unwrap();
        assert_eq!(head[1], 0, "domain CONNECT must succeed");

        let mut rest = [0u8; 6];
        client.read_exact(&mut rest).unwrap();
        client.write_all(b"x").unwrap();
        let mut buf = [0u8; 1];
        client.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"x");

        proxy.stop();
    }

    #[test]
    fn rejects_non_socks_clients() {
        let mut proxy = Proxy::start(Options::default()).expect("start proxy");
        let mut client = TcpStream::connect(proxy.addr).expect("connect proxy");
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        let mut buf = [0u8; 4];
        // The connection is closed without a SOCKS reply.
        assert!(matches!(client.read(&mut buf), Ok(0) | Err(_)));
        proxy.stop();
    }

    #[test]
    fn unsupported_command_gets_a_socks_error() {
        let mut proxy = Proxy::start(Options::default()).expect("start proxy");
        let mut client = TcpStream::connect(proxy.addr).expect("connect proxy");
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        client.write_all(&[5, 1, 0]).unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).unwrap();
        client.write_all(&[5, 2, 0, 1, 127, 0, 0, 1, 0, 80]).unwrap(); // BIND
        let mut head = [0u8; 2];
        client.read_exact(&mut head).unwrap();
        assert_eq!(head, [5, 7], "command not supported");
        proxy.stop();
    }

    #[test]
    fn url_is_socks5() {
        let addr: SocketAddr = "127.0.0.1:1080".parse().unwrap();
        assert_eq!(proxy_url(addr), "socks5://127.0.0.1:1080");
    }
}
