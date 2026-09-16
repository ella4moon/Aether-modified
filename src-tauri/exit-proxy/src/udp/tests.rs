use super::*;
use std::sync::mpsc;

fn tcp() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").unwrap()
}
fn udp() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    s
}
fn socket(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    s
}
fn address(addr: SocketAddr) -> Target {
    Target {
        host: addr.ip().to_string(),
        port: addr.port(),
    }
}
fn frame(target: &Target, payload: &[u8]) -> Vec<u8> {
    let mut p = prefix(target);
    p.extend(payload);
    p
}
fn expect(stream: &mut TcpStream, bytes: &[u8]) {
    let mut actual = vec![0; bytes.len()];
    stream.read_exact(&mut actual).unwrap();
    assert_eq!(actual, bytes);
}
fn login(stream: &mut TcpStream, auth: bool) {
    let method = if auth { 2 } else { 0 };
    expect(stream, &[5, 1, method]);
    stream.write_all(&[5, method]).unwrap();
    if auth {
        expect(stream, b"\x01\x04user\x04pass");
        stream.write_all(&[1, 0]).unwrap();
    }
}
fn request(stream: &mut TcpStream, cmd: u8) -> Target {
    let header = read_array::<4>(stream).unwrap();
    assert_eq!(&header[..3], &[5, cmd, 0]);
    read_target(stream, header[3]).unwrap()
}
fn bound(stream: &mut TcpStream, target: &Target) {
    let mut p = vec![5, 0, 0];
    p.extend(target.encode());
    stream.write_all(&p).unwrap();
}
fn client(addr: SocketAddr, source: SocketAddr) -> TcpStream {
    let mut client = socket(addr);
    client.write_all(&[5, 1, 0]).unwrap();
    expect(&mut client, &[5, 0]);
    let mut p = vec![5, 3, 0];
    p.extend(address(source).encode());
    client.write_all(&p).unwrap();
    client
}
fn success(client: &mut TcpStream) -> SocketAddr {
    let h = read_array::<4>(client).unwrap();
    assert_eq!(&h[..3], &[5, 0, 0]);
    let b = read_target(client, h[3]).unwrap();
    SocketAddr::new(b.host.parse().unwrap(), b.port)
}

struct Fixture {
    server: Option<Server>,
    addr: SocketAddr,
    controls: mpsc::Receiver<Vec<TcpStream>>,
    tcp_worker: JoinHandle<()>,
    core_udp: UdpSocket,
    final_udp: UdpSocket,
    advertised: Target,
}

impl Fixture {
    // refuse=1 rejects at final proxy, refuse=2 rejects at Aether.
    fn new(auth: bool, unspecified: bool, refuse: u8) -> Self {
        let core = tcp();
        let core_addr = core.local_addr().unwrap();
        let core_udp = udp();
        let core_udp_addr = core_udp.local_addr().unwrap();
        let final_udp = udp();
        let final_udp_addr = final_udp.local_addr().unwrap();
        let advertised = Target {
            host: if unspecified {
                "exit.test".into()
            } else {
                final_udp_addr.ip().to_string()
            },
            port: final_udp_addr.port(),
        };
        let (tx, controls) = mpsc::channel();
        let tcp_worker = thread::spawn(move || {
            let (mut final_control, _) = core.accept().unwrap();
            final_control
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            login(&mut final_control, false);
            assert_eq!(
                request(&mut final_control, 1),
                Target::new("exit.test", 1080).unwrap()
            );
            reply(&mut final_control, 0).unwrap();
            login(&mut final_control, auth);
            assert_eq!(
                request(&mut final_control, 3),
                Target {
                    host: "0.0.0.0".into(),
                    port: 0
                }
            );
            if refuse == 1 {
                reply(&mut final_control, 7).unwrap();
                tx.send(vec![final_control]).unwrap();
                return;
            }
            bound(
                &mut final_control,
                &Target {
                    host: if unspecified {
                        "0.0.0.0".into()
                    } else {
                        final_udp_addr.ip().to_string()
                    },
                    port: final_udp_addr.port(),
                },
            );
            let (mut core_control, _) = core.accept().unwrap();
            core_control
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            login(&mut core_control, false);
            let source = request(&mut core_control, 3);
            assert_eq!(source.host, "127.0.0.1");
            assert_ne!(source.port, 0);
            if refuse == 2 {
                reply(&mut core_control, 7).unwrap();
            } else {
                bound(&mut core_control, &address(core_udp_addr));
            }
            tx.send(vec![final_control, core_control]).unwrap();
        });
        let bind = tcp();
        let addr = bind.local_addr().unwrap();
        let server = Server::start(
            bind,
            core_addr,
            Config {
                kind: Kind::Socks5,
                host: "exit.test".into(),
                port: 1080,
                udp_enabled: true,
                credentials: auth.then(|| Credentials {
                    username: "user".into(),
                    password: "pass".into(),
                }),
            },
            Arc::new(|_| {}),
        )
        .unwrap();
        Self {
            server: Some(server),
            addr,
            controls,
            tcp_worker,
            core_udp,
            final_udp,
            advertised,
        }
    }

    fn round_trip(&self, app: &UdpSocket, local: SocketAddr, payload: &[u8]) {
        let destination = Target::new("game.test", 27015).unwrap();
        app.send_to(&frame(&destination, payload), local).unwrap();
        let mut buf = vec![0; 65_535];
        let (n, app_transport) = self.core_udp.recv_from(&mut buf).unwrap();
        let (relay, inner) = decode(&buf[..n]).unwrap();
        assert_eq!(
            relay, self.advertised,
            "Aether must receive a frame addressed to the final relay"
        );
        assert_eq!(decode(inner).unwrap(), (destination.clone(), payload));
        self.core_udp
            .send_to(inner, self.final_udp.local_addr().unwrap())
            .unwrap();
        let (n, from) = self.final_udp.recv_from(&mut buf).unwrap();
        assert_eq!(
            from,
            self.core_udp.local_addr().unwrap(),
            "final UDP must arrive from Aether"
        );
        assert_eq!(decode(&buf[..n]).unwrap(), (destination, payload));
        let source = Target::new("198.51.100.9", 27015).unwrap();
        let mut result = b"final-exit:".to_vec();
        result.extend(payload);
        self.final_udp
            .send_to(&frame(&source, &result), from)
            .unwrap();
        let (n, from) = self.core_udp.recv_from(&mut buf).unwrap();
        assert_eq!(from, self.final_udp.local_addr().unwrap());
        self.core_udp
            .send_to(&frame(&address(from), &buf[..n]), app_transport)
            .unwrap();
        let (n, from) = app.recv_from(&mut buf).unwrap();
        assert_eq!(from, local);
        assert_eq!(decode(&buf[..n]).unwrap(), (source, result.as_slice()));
    }
}

#[test]
fn udp_chain_preserves_both_hops_authentication_domains_and_datagram_boundaries() {
    for (auth, unspecified) in [(false, false), (true, false), (true, true)] {
        let f = Fixture::new(auth, unspecified, 0);
        let app = udp();
        let mut control = client(f.addr, app.local_addr().unwrap());
        let local = success(&mut control);
        let mut controls = f.controls.recv_timeout(Duration::from_secs(3)).unwrap();
        for payload in [&b""[..], &b"voice packet\0\x01"[..], &[42u8; 1200][..]] {
            f.round_trip(&app, local, payload);
        }
        drop(control);
        for c in &mut controls {
            assert_eq!(c.read(&mut [0u8; 1]).unwrap(), 0);
        }
        f.tcp_worker.join().unwrap();
        drop(f.server);
    }
}

#[test]
fn udp_rejects_unexpected_sources_fragments_and_malformed_packets() {
    let f = Fixture::new(false, false, 0);
    let app = udp();
    let other = udp();
    let mut control = client(f.addr, app.local_addr().unwrap());
    let local = success(&mut control);
    let _controls = f.controls.recv_timeout(Duration::from_secs(3)).unwrap();
    let valid = frame(&Target::new("game.test", 27015).unwrap(), b"bad-source");
    other.send_to(&valid, local).unwrap();
    for bad in [
        &[0, 0, 1, 1, 1, 1, 1, 1, 0, 53][..],
        &[9, 0, 0, 1, 1, 1, 1, 1, 0, 53][..],
        &[0, 0][..],
    ] {
        app.send_to(bad, local).unwrap();
    }
    // The first datagram reaching Aether must be this valid one.
    f.round_trip(&app, local, b"valid");
    f.tcp_worker.join().unwrap();
}

#[test]
fn failure_of_either_udp_association_fails_without_direct_datagrams() {
    for refuse in [1, 2] {
        let f = Fixture::new(true, false, refuse);
        let app = udp();
        let mut control = client(f.addr, app.local_addr().unwrap());
        let h = read_array::<4>(&mut control).unwrap();
        assert_ne!(h[1], 0);
        read_target(&mut control, h[3]).unwrap();
        let mut controls = f.controls.recv_timeout(Duration::from_secs(3)).unwrap();
        for c in &mut controls {
            // A refused SOCKS response can leave its unused BND fields unread.
            // Windows may report the resulting close as a reset rather than EOF.
            // Both close the association; data or a timeout must still fail.
            match c.read(&mut [0u8; 1]) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) => {}
                other => panic!("Rejected association did not close: {other:?}"),
            }
        }
        for socket in [&f.core_udp, &f.final_udp] {
            socket.set_nonblocking(true).unwrap();
            assert_eq!(
                socket.recv_from(&mut [0u8; 16]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        f.tcp_worker.join().unwrap();
    }
}

#[test]
fn disconnect_or_either_upstream_control_closing_stops_udp() {
    for close in 0..3 {
        let mut f = Fixture::new(false, false, 0);
        let app = udp();
        let mut control = client(f.addr, app.local_addr().unwrap());
        let local = success(&mut control);
        let controls = f.controls.recv_timeout(Duration::from_secs(3)).unwrap();
        if close == 2 {
            drop(f.server.take());
        } else {
            controls[close].shutdown(Shutdown::Both).unwrap();
        }
        assert_eq!(control.read(&mut [0u8; 1]).unwrap(), 0);
        // A TCP close invalidates its UDP association, including the bound port.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if UdpSocket::bind(local).is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "UDP socket must close with the association"
            );
            thread::sleep(Duration::from_millis(10));
        }
        f.tcp_worker.join().unwrap();
    }
}

#[test]
fn datagram_codec_handles_ip_families_and_rejects_truncated_frames() {
    for host in ["203.0.113.9", "2001:db8::9", "game.test"] {
        let target = Target::new(host, 443).unwrap();
        let packet = frame(&target, b"");
        assert_eq!(decode(&packet), Some((target, &b""[..])));
        for end in 0..packet.len() {
            assert!(decode(&packet[..end]).is_none());
        }
    }
    assert!(decode(&[0, 0, 0, 3, 0, 0, 53]).is_none());
    assert!(decode(&[0, 0, 0, 1, 1, 1, 1, 1, 0, 0]).is_none());
}

#[test]
fn aether_cannot_redirect_the_udp_transport_to_a_remote_address() {
    let core: SocketAddr = "127.0.0.1:1819".parse().unwrap();
    for host in ["192.0.2.1", "relay.test", "127.0.0.2"] {
        assert!(local_relay(Target::new(host, 1234).unwrap(), core).is_err());
    }
    assert_eq!(
        local_relay(Target::new("0.0.0.0", 1234).unwrap(), core).unwrap(),
        "127.0.0.1:1234".parse().unwrap()
    );
    assert!(local_relay(
        Target {
            host: "127.0.0.1".into(),
            port: 0
        },
        core
    )
    .is_err());
}

#[test]
fn http_connect_cannot_be_mislabelled_as_udp_capable() {
    assert!(Config {
        kind: Kind::Http,
        host: "exit.test".into(),
        port: 8080,
        credentials: None,
        udp_enabled: true
    }
    .validate()
    .is_err());
}

#[test]
fn connection_probe_checks_udp_when_enabled() {
    let core = tcp();
    let core_addr = core.local_addr().unwrap();
    let fake = thread::spawn(move || {
        // First, the normal TCP connection check.
        let (mut c, _) = core.accept().unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        login(&mut c, false);
        assert_eq!(request(&mut c, 1), Target::new("exit.test", 1080).unwrap());
        reply(&mut c, 0).unwrap();
        login(&mut c, false);
        assert_eq!(request(&mut c, 1), Target::new("example.com", 443).unwrap());
        reply(&mut c, 0).unwrap();
        assert_eq!(c.read(&mut [0u8; 1]).unwrap(), 0);
        // TCP works, but this provider refuses a UDP association.
        let (mut c, _) = core.accept().unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        login(&mut c, false);
        request(&mut c, 1);
        reply(&mut c, 0).unwrap();
        login(&mut c, false);
        request(&mut c, 3);
        reply(&mut c, 7).unwrap();
    });
    let server = Server::start(
        tcp(),
        core_addr,
        Config {
            kind: Kind::Socks5,
            host: "exit.test".into(),
            port: 1080,
            credentials: None,
            udp_enabled: true,
        },
        Arc::new(|_| {}),
    )
    .unwrap();
    assert!(server
        .probe()
        .verify()
        .unwrap_err()
        .to_string()
        .contains("UDP ASSOCIATE"));
    fake.join().unwrap();
}

#[test]
fn disconnect_cancels_an_incomplete_udp_handshake() {
    let core = tcp();
    let core_addr = core.local_addr().unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let fake = thread::spawn(move || {
        let (mut c, _) = core.accept().unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        login(&mut c, false);
        request(&mut c, 1);
        reply(&mut c, 0).unwrap();
        login(&mut c, false);
        request(&mut c, 3);
        waiting_tx.send(()).unwrap();
        assert_eq!(c.read(&mut [0u8; 1]).unwrap(), 0);
    });
    let server = Server::start(
        tcp(),
        core_addr,
        Config {
            kind: Kind::Socks5,
            host: "exit.test".into(),
            port: 1080,
            credentials: None,
            udp_enabled: true,
        },
        Arc::new(|_| {}),
    )
    .unwrap();
    let shared = server.shared.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let check = thread::spawn(move || {
        done_tx.send(Association::open(&shared).is_err()).unwrap();
    });
    waiting_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(server);
    assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    check.join().unwrap();
    fake.join().unwrap();
}
