//! A local SOCKS5 CONNECT/UDP ASSOCIATE listener with exactly two proxy hops:
//! Aether's loopback SOCKS5 listener, then the configured final proxy.
//! No destination or final-proxy address is resolved or dialled locally.

mod udp;

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CLIENTS: usize = 128;
const MAX_HTTP_HEAD: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Socks5,
    Http,
}

#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials([redacted])")
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    pub credentials: Option<Credentials>,
    pub udp_enabled: bool,
}

impl Config {
    pub fn validate(&self) -> io::Result<()> {
        Target::new(&self.host, self.port)?;
        if self.udp_enabled && self.kind != Kind::Socks5 {
            return Err(invalid("UDP requires a SOCKS5 final proxy with UDP ASSOCIATE support. HTTP CONNECT carries TCP only."));
        }
        if let Some(auth) = &self.credentials {
            if auth.username.is_empty() || auth.password.is_empty() {
                return Err(invalid("Enter both the final proxy username and password"));
            }
            if auth.username.len() > 255 || auth.password.len() > 255 {
                return Err(invalid(
                    "Proxy credentials must each fit in 255 UTF-8 bytes",
                ));
            }
            if self.kind == Kind::Http && auth.username.contains(':') {
                return Err(invalid("An HTTP proxy username cannot contain a colon"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
}

impl Target {
    fn new(host: &str, port: u16) -> io::Result<Self> {
        let host = host.trim();
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        if port == 0 || host.is_empty() || host.len() > 255 {
            return Err(invalid(
                "A valid proxy host and port (1–65535) are required",
            ));
        }
        if host.parse::<IpAddr>().is_err()
            && !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        {
            return Err(invalid(
                "Use a hostname or IP address, without a URL, path, or credentials",
            ));
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    fn authority(&self) -> String {
        if matches!(self.host.parse::<IpAddr>(), Ok(IpAddr::V6(_))) {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(260);
        match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => {
                bytes.push(1);
                bytes.extend(ip.octets());
            }
            Ok(IpAddr::V6(ip)) => {
                bytes.push(4);
                bytes.extend(ip.octets());
            }
            Err(_) => {
                bytes.extend([3, self.host.len() as u8]);
                bytes.extend(self.host.as_bytes());
            }
        }
        bytes.extend(self.port.to_be_bytes());
        bytes
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn read_array<const N: usize>(stream: &mut TcpStream) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_target(stream: &mut TcpStream, atyp: u8) -> io::Result<Target> {
    let host = match atyp {
        1 => std::net::Ipv4Addr::from(read_array::<4>(stream)?).to_string(),
        4 => std::net::Ipv6Addr::from(read_array::<16>(stream)?).to_string(),
        3 => {
            let len = read_array::<1>(stream)?[0] as usize;
            let mut bytes = vec![0; len];
            stream.read_exact(&mut bytes)?;
            String::from_utf8(bytes).map_err(|_| invalid("SOCKS hostname must be ASCII"))?
        }
        _ => return Err(invalid("Unsupported SOCKS address type")),
    };
    Ok(Target {
        host,
        port: u16::from_be_bytes(read_array::<2>(stream)?),
    })
}

fn socks_login(stream: &mut TcpStream, credentials: Option<&Credentials>) -> io::Result<()> {
    let method = if credentials.is_some() { 2 } else { 0 };
    stream.write_all(&[5, 1, method])?;
    if read_array::<2>(stream)? != [5, method] {
        return Err(refused(
            "The SOCKS5 proxy rejected the selected authentication method",
        ));
    }
    if let Some(auth) = credentials {
        let mut packet = vec![1, auth.username.len() as u8];
        packet.extend(auth.username.as_bytes());
        packet.push(auth.password.len() as u8);
        packet.extend(auth.password.as_bytes());
        stream.write_all(&packet)?;
        if read_array::<2>(stream)? != [1, 0] {
            return Err(refused("The final proxy rejected the username or password"));
        }
    }
    Ok(())
}

fn socks_connect(stream: &mut TcpStream, target: &Target) -> io::Result<()> {
    socks_request(stream, 1, target)?;
    Ok(())
}

fn socks_request(stream: &mut TcpStream, command: u8, target: &Target) -> io::Result<Target> {
    let mut packet = vec![5, command, 0];
    packet.extend(target.encode());
    stream.write_all(&packet)?;
    let response = read_array::<4>(stream)?;
    if response[0] != 5 || response[2] != 0 {
        return Err(invalid("Malformed SOCKS5 proxy response"));
    }
    if response[1] != 0 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!(
                "The proxy refused {} (SOCKS code {})",
                if command == 3 {
                    "UDP ASSOCIATE"
                } else {
                    "CONNECT"
                },
                response[1]
            ),
        ));
    }
    // Consume only the response; any coalesced application bytes stay in TCP.
    read_target(stream, response[3])
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn http_connect(
    stream: &mut TcpStream,
    target: &Target,
    auth: Option<&Credentials>,
) -> io::Result<()> {
    let authority = target.authority();
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = auth {
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&base64(
            format!("{}:{}", auth.username, auth.password).as_bytes(),
        ));
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    let mut total = 0;
    // Proxies may send interim 1xx responses before the CONNECT result.
    loop {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if total >= MAX_HTTP_HEAD {
                return Err(invalid("HTTP proxy response headers are too large"));
            }
            header.push(read_array::<1>(stream)?[0]);
            total += 1;
        }
        let first = header.split(|b| *b == b'\n').next().unwrap_or_default();
        let first =
            std::str::from_utf8(first).map_err(|_| invalid("Malformed HTTP proxy status"))?;
        let mut fields = first.split_ascii_whitespace();
        if !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
            return Err(invalid("The final proxy did not return an HTTP response"));
        }
        let code = fields
            .next()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(0);
        match code {
            200..=299 => return Ok(()),
            100..=199 if code != 101 => continue,
            407 => {
                return Err(refused(
                    "The HTTP proxy rejected or requires authentication",
                ))
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("The HTTP proxy refused CONNECT (status {code})"),
                ))
            }
        }
    }
}

struct Shared {
    stopped: AtomicBool,
    next_id: AtomicU64,
    clients: AtomicUsize,
    sockets: Mutex<HashMap<u64, SocketEntry>>,
    core: SocketAddr,
    config: Config,
}

struct SocketEntry {
    socket: TcpStream,
    deadline: Option<Instant>,
}

struct Tracked {
    stream: TcpStream,
    id: u64,
    shared: Arc<Shared>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
        self.shared.sockets.lock().unwrap().remove(&self.id);
    }
}

impl Tracked {
    fn handshake_complete(&self) -> io::Result<()> {
        self.stream.set_read_timeout(None)?;
        self.stream.set_write_timeout(None)?;
        if let Some(entry) = self.shared.sockets.lock().unwrap().get_mut(&self.id) {
            entry.deadline = None;
        }
        Ok(())
    }
}

impl Shared {
    fn track(self: &Arc<Self>, stream: TcpStream) -> io::Result<Tracked> {
        let mut sockets = self.sockets.lock().unwrap();
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Proxy stopped"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        sockets.insert(
            id,
            SocketEntry {
                socket: stream.try_clone()?,
                deadline: Some(Instant::now() + HANDSHAKE_TIMEOUT),
            },
        );
        Ok(Tracked {
            stream,
            id,
            shared: self.clone(),
        })
    }

    fn connect_core(self: &Arc<Self>) -> io::Result<Tracked> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Proxy stopped"));
        }
        // The only TCP dial is to Aether. UDP similarly uses only Aether's
        // verified loopback UDP relay, never a final proxy or destination.
        let stream = TcpStream::connect_timeout(&self.core, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let mut tracked = self.track(stream)?;
        socks_login(&mut tracked.stream, None)?;
        Ok(tracked)
    }

    fn connect_final(self: &Arc<Self>) -> io::Result<Tracked> {
        let mut tracked = self.connect_core()?;
        socks_connect(
            &mut tracked.stream,
            &Target::new(&self.config.host, self.config.port)?,
        )?;
        Ok(tracked)
    }

    fn connect(self: &Arc<Self>, target: &Target) -> io::Result<Tracked> {
        let mut tracked = self.connect_final()?;
        let stream = &mut tracked.stream;
        match self.config.kind {
            Kind::Socks5 => {
                socks_login(stream, self.config.credentials.as_ref())?;
                socks_connect(stream, target)?;
            }
            Kind::Http => http_connect(stream, target, self.config.credentials.as_ref())?,
        }
        tracked.handshake_complete()?;
        Ok(tracked)
    }

    fn expire_handshakes(&self) {
        let now = Instant::now();
        for entry in self.sockets.lock().unwrap().values_mut() {
            if entry.deadline.is_some_and(|deadline| now >= deadline) {
                let _ = entry.socket.shutdown(Shutdown::Both);
                entry.deadline = None;
            }
        }
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let mut sockets = self.sockets.lock().unwrap();
        for entry in sockets.values() {
            let _ = entry.socket.shutdown(Shutdown::Both);
        }
        sockets.clear();
    }
}

/// A cancellable startup check; does not need to hold the GUI manager's lock.
#[derive(Clone)]
pub struct Probe(Arc<Shared>);

impl Probe {
    /// Ask the final proxy for a TCP connection to example.com:443. Sends no
    /// application data. Authentication and both CONNECT hops must succeed.
    pub fn verify(&self) -> io::Result<()> {
        self.0.connect(&Target::new("example.com", 443)?)?;
        if self.0.config.udp_enabled {
            // Verify both proxies accept UDP associations. This checks the
            // capability/control path, not reachability of every UDP service.
            udp::Association::open(&self.0)?;
        }
        Ok(())
    }
}

pub struct Server {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(
        listener: TcpListener,
        core: SocketAddr,
        config: Config,
        on_error: Arc<dyn Fn(String) + Send + Sync>,
    ) -> io::Result<Self> {
        config.validate()?;
        if !core.ip().is_loopback() || core.port() == 0 {
            return Err(invalid("The Aether listener must be a loopback address"));
        }
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            stopped: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            clients: AtomicUsize::new(0),
            sockets: Mutex::new(HashMap::new()),
            core,
            config,
        });
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("final-proxy-listener".into())
            .spawn(move || {
                while !state.stopped.load(Ordering::Acquire) {
                    state.expire_handshakes();
                    match listener.accept() {
                        Ok((client, _)) => {
                            if state.clients.load(Ordering::Relaxed) >= MAX_CLIENTS {
                                continue;
                            }
                            let client = match state.track(client) {
                                Ok(c) => c,
                                Err(_) => break,
                            };
                            state.clients.fetch_add(1, Ordering::Relaxed);
                            let shared = state.clone();
                            let report = on_error.clone();
                            let guard = ClientCount(shared.clone());
                            let _ = thread::Builder::new()
                                .name("final-proxy-client".into())
                                .spawn(move || {
                                    let _guard = guard;
                                    if let Err(error) = handle_client(client, &shared) {
                                        if !shared.stopped.load(Ordering::Acquire)
                                            && error.kind() != io::ErrorKind::UnexpectedEof
                                        {
                                            report(format!("Final proxy: {error}"));
                                        }
                                    }
                                });
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20))
                        }
                        Err(_) => thread::sleep(Duration::from_millis(100)),
                    }
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(worker),
        })
    }

    pub fn probe(&self) -> Probe {
        Probe(self.shared.clone())
    }
}

struct ClientCount(Arc<Shared>);
impl Drop for ClientCount {
    fn drop(&mut self) {
        self.0.clients.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

fn reply(stream: &mut TcpStream, code: u8) -> io::Result<()> {
    stream.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0])
}

fn handle_client(mut client: Tracked, shared: &Arc<Shared>) -> io::Result<()> {
    // macOS and Windows can inherit nonblocking mode from the listener.
    // Each client has a dedicated worker using blocking reads and writes.
    client.stream.set_nonblocking(false)?;
    client.stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    client.stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    client.stream.set_nodelay(true)?;
    let greeting = read_array::<2>(&mut client.stream)?;
    if greeting[0] != 5 {
        return Err(invalid("This listener accepts SOCKS5 clients"));
    }
    let mut methods = vec![0; greeting[1] as usize];
    client.stream.read_exact(&mut methods)?;
    if !methods.contains(&0) {
        client.stream.write_all(&[5, 255])?;
        return Err(refused("The local SOCKS5 listener uses no authentication"));
    }
    client.stream.write_all(&[5, 0])?;
    let request = read_array::<4>(&mut client.stream)?;
    if request[0] != 5 || request[2] != 0 {
        reply(&mut client.stream, 1)?;
        return Err(invalid("Malformed SOCKS5 request"));
    }
    if request[1] == 3 && shared.config.udp_enabled {
        let requested = read_target(&mut client.stream, request[3])?;
        return udp::handle(client, shared, requested);
    }
    if request[1] != 1 {
        reply(&mut client.stream, 7)?;
        return Err(invalid(
            "Enable UDP with a SOCKS5 final proxy to use UDP ASSOCIATE. SOCKS BIND is not supported.",
        ));
    }
    let target = match read_target(&mut client.stream, request[3])
        .and_then(|t| Target::new(&t.host, t.port))
    {
        Ok(t) => t,
        Err(error) => {
            reply(&mut client.stream, 8)?;
            return Err(error);
        }
    };
    let remote = match shared.connect(&target) {
        Ok(remote) => remote,
        Err(error) => {
            let _ = reply(&mut client.stream, 5);
            return Err(error);
        }
    };
    reply(&mut client.stream, 0)?;
    client.handshake_complete()?;
    relay(client, remote)
}

fn relay(mut client: Tracked, mut remote: Tracked) -> io::Result<()> {
    let mut upload_in = client.stream.try_clone()?;
    let mut upload_out = remote.stream.try_clone()?;
    thread::scope(|scope| {
        let upload = scope.spawn(move || {
            let result = io::copy(&mut upload_in, &mut upload_out);
            let _ = upload_out.shutdown(if result.is_ok() {
                Shutdown::Write
            } else {
                Shutdown::Both
            });
            if result.is_err() {
                let _ = upload_in.shutdown(Shutdown::Both);
            }
            result
        });
        let download = io::copy(&mut remote.stream, &mut client.stream);
        let _ = client.stream.shutdown(if download.is_ok() {
            Shutdown::Write
        } else {
            Shutdown::Both
        });
        if download.is_err() {
            let _ = remote.stream.shutdown(Shutdown::Both);
        }
        let upload = upload
            .join()
            .map_err(|_| io::Error::other("Proxy relay worker stopped"))?;
        upload?;
        download?;
        Ok(())
    })
}

#[cfg(test)]
mod tests;
