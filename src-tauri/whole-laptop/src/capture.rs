use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// IANA benchmarking space, never an Internet destination. Only this address,
// TCP, and one ephemeral port get the loopback-only startup-test outbound.
pub const CAPTURE_TEST_IP: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 43);

pub struct CaptureProbe {
    listener: TcpListener,
}

impl CaptureProbe {
    pub fn new() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener })
    }

    pub fn configure(&self, config: &mut Value) -> io::Result<()> {
        let port = self.listener.local_addr()?.port();
        let outbounds = config["outbounds"].as_array_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing tunnel outbounds")
        })?;
        outbounds.push(json!({
            "type": "direct", "tag": "capture-self-test",
            "inet4_bind_address": "127.0.0.1"
        }));
        let rules = config["route"]["rules"].as_array_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing tunnel rules")
        })?;
        rules.insert(0, json!({
            "ip_cidr": [format!("{CAPTURE_TEST_IP}/32")], "network": "tcp",
            "port": port, "action": "route", "outbound": "capture-self-test",
            "override_address": "127.0.0.1", "override_port": port
        }));
        Ok(())
    }

    pub fn verify(&self) -> io::Result<()> {
        self.verify_at(SocketAddr::from((CAPTURE_TEST_IP, self.listener.local_addr()?.port())))
    }

    fn verify_at(&self, destination: SocketAddr) -> io::Result<()> {
        let stop = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(8);
        let nonce = format!("aether-capture:{}:{}:{}", std::process::id(),
            self.listener.local_addr()?.port(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
        std::thread::scope(|scope| {
            // A nonblocking listener plus bounded reads also makes the failure
            // path bounded when no SYN ever reaches the capture stack.
            let echo = scope.spawn(|| -> io::Result<()> {
                while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
                    match self.listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false)?;
                            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                            let mut received = vec![0; nonce.len()];
                            stream.read_exact(&mut received)?;
                            if received != nonce.as_bytes() {
                                return Err(io::Error::new(io::ErrorKind::InvalidData, "Wrong capture-test payload"));
                            }
                            stream.write_all(&received)?;
                            return Ok(());
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(io::Error::new(io::ErrorKind::TimedOut, "No captured TCP connection arrived"))
            });
            let result = (|| {
                let mut stream = TcpStream::connect_timeout(&destination, Duration::from_secs(3))?;
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(nonce.as_bytes())?;
                let mut reply = vec![0; nonce.len()];
                stream.read_exact(&mut reply)?;
                if reply != nonce.as_bytes() {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Capture-test reply did not match"));
                }
                Ok(())
            })();
            stop.store(true, Ordering::Release);
            let echoed = echo.join().unwrap_or_else(|_| Err(io::Error::other("Capture-test worker stopped")));
            result.and(echoed)
        })
    }
}

/// Sends a real DNS query into the TUN's DNS listener. In production the reply
/// requires a complete DNS-over-HTTPS round trip through Aether and the final
/// proxy *after* the Windows capture routes are active. TCP-only mode supports
/// this too: the intercepted local UDP query becomes upstream HTTPS/TCP.
pub fn check_dns(server: SocketAddr) -> io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect(server)?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    socket.set_read_timeout(Some(Duration::from_secs(12)))?;
    let id = socket.local_addr()?.port();
    let mut query = vec![0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    query[..2].copy_from_slice(&id.to_be_bytes());
    query.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
    socket.send(&query)?;
    let mut response = [0; 2048];
    let n = socket.recv(&mut response)?;
    validate_dns_response(&query, &response[..n])
}

fn validate_dns_response(query: &[u8], response: &[u8]) -> io::Result<()> {
    if response.len() < query.len() + 12 || response[..2] != query[..2]
        || response[2] & 0x80 == 0 || response[2] & 0x02 != 0
        || response[3] & 0x0f != 0 || response[4..6] != [0, 1]
        || response[6..8] == [0, 0] || response[12..query.len()] != query[12..]
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData,
            "The tunnel DNS check did not receive a successful answer for example.com"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_probe_can_only_route_one_benchmark_tcp_port_to_loopback() {
        let probe = CaptureProbe::new().unwrap();
        let mut config = json!({"outbounds": [], "route": {"rules": [], "final": "local-final-proxy"}});
        probe.configure(&mut config).unwrap();
        let rule = &config["route"]["rules"][0];
        let outbound = &config["outbounds"][0];
        assert_eq!(rule["ip_cidr"], json!(["198.18.0.43/32"]));
        assert_eq!(rule["network"], "tcp");
        assert_eq!(rule["port"], rule["override_port"]);
        assert_eq!(rule["override_address"], "127.0.0.1");
        assert_eq!(outbound["inet4_bind_address"], "127.0.0.1");
        assert_eq!(config["route"]["final"], "local-final-proxy");
    }

    #[test]
    fn capture_probe_requires_echoed_data_and_stops_after_connection_failure() {
        let probe = CaptureProbe::new().unwrap();
        probe.verify_at(probe.listener.local_addr().unwrap()).unwrap();
        let unused = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = unused.local_addr().unwrap();
        drop(unused);
        let before = Instant::now();
        assert!(probe.verify_at(addr).is_err());
        assert!(before.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn dns_probe_rejects_empty_refused_and_unrelated_responses() {
        let query = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01";
        let mut response = query.to_vec();
        response[2] = 0x81;
        response[7] = 1;
        response.extend_from_slice(b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04\xc6\x12\x00\x2a");
        assert!(validate_dns_response(query, &response).is_ok());
        for index in [0, 3, 7, 13] {
            let mut bad = response.clone();
            bad[index] ^= 1;
            assert!(validate_dns_response(query, &bad).is_err());
        }
        assert!(validate_dns_response(query, &[]).is_err());
    }
}
