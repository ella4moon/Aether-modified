use super::*;
use std::sync::mpsc;

fn listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").unwrap()
}

fn socket(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn expected(stream: &mut TcpStream, bytes: &[u8]) {
    let mut actual = vec![0; bytes.len()];
    stream.read_exact(&mut actual).unwrap();
    assert_eq!(actual, bytes);
}

fn connect_packet(host: &str, port: u16) -> Vec<u8> {
    let mut packet = vec![5, 1, 0, 3, host.len() as u8];
    packet.extend_from_slice(host.as_bytes());
    packet.extend_from_slice(&port.to_be_bytes());
    packet
}

fn bridge(mut a: TcpStream, mut b: TcpStream) {
    let mut from_a = a.try_clone().unwrap();
    let mut to_b = b.try_clone().unwrap();
    let upload = thread::spawn(move || {
        let _ = io::copy(&mut from_a, &mut to_b);
        let _ = to_b.shutdown(Shutdown::Write);
    });
    let _ = io::copy(&mut b, &mut a);
    let _ = a.shutdown(Shutdown::Write);
    upload.join().unwrap();
}

// An independent fake Aether listener. Its only allowed CONNECT destination
// is exit.test, a name the relay must pass through SOCKS without resolving.
fn outer(exit: SocketAddr) -> (SocketAddr, JoinHandle<()>) {
    let bind = listener();
    let addr = bind.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut client, _) = bind.accept().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        expected(&mut client, &[5, 1, 0]);
        client.write_all(&[5, 0]).unwrap();
        expected(&mut client, &connect_packet("exit.test", exit.port()));
        let remote = socket(exit);
        // A domain-form bound address tests response consumption as well.
        client.write_all(&[5, 0, 0, 3, 1, b'x', 0, 0]).unwrap();
        bridge(client, remote);
    });
    (addr, worker)
}

fn start(
    core: SocketAddr,
    kind: Kind,
    port: u16,
    auth: Option<Credentials>,
) -> (Server, SocketAddr) {
    let bind = listener();
    let addr = bind.local_addr().unwrap();
    let server = Server::start(
        bind,
        core,
        Config {
            udp_enabled: false,
            kind,
            host: "exit.test".into(),
            port,
            credentials: auth,
        },
        Arc::new(|_| {}),
    )
    .unwrap();
    (server, addr)
}

fn client_connect(addr: SocketAddr) -> TcpStream {
    let mut client = socket(addr);
    client.write_all(&[5, 1, 0]).unwrap();
    expected(&mut client, &[5, 0]);
    client
        .write_all(&connect_packet("website.test", 443))
        .unwrap();
    client
}

fn exchange(mut stream: TcpStream) {
    // The proxy sends early bytes with the CONNECT success, then waits for
    // a client half-close before producing the rest of the response.
    let mut hello = vec![5, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    hello.extend_from_slice(b"EARLY");
    stream.write_all(&hello).unwrap();
    let mut body = Vec::new();
    stream.read_to_end(&mut body).unwrap();
    assert_eq!(body, b"payload");
    stream.write_all(b"FINAL-EXIT").unwrap();
}

fn assert_exchange(mut client: TcpStream) {
    expected(&mut client, &[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    expected(&mut client, b"EARLY");
    client.write_all(b"payload").unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    client.read_to_end(&mut reply).unwrap();
    assert_eq!(reply, b"FINAL-EXIT");
}

#[test]
fn accepted_nonblocking_socket_waits_for_fragmented_handshake() {
    let exit = listener();
    let exit_addr = exit.local_addr().unwrap();
    let fake_exit = thread::spawn(move || {
        let (mut stream, _) = exit.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        expected(&mut stream, &[5, 1, 0]);
        stream.write_all(&[5, 0]).unwrap();
        expected(&mut stream, &connect_packet("website.test", 443));
        exchange(stream);
    });
    let (core, fake_core) = outer(exit_addr);
    let (server, _) = start(core, Kind::Socks5, exit_addr.port(), None);

    let bind = listener();
    let mut client = socket(bind.local_addr().unwrap());
    let (accepted, _) = bind.accept().unwrap();
    // Emulate platforms that inherit the listener's nonblocking mode.
    accepted.set_nonblocking(true).unwrap();
    let tracked = server.shared.track(accepted).unwrap();
    let shared = server.shared.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = done_tx.send(handle_client(tracked, &shared));
    });

    // The handler must wait for a client, then for the rest of a partial greeting.
    for fragment in [&[][..], &[5][..]] {
        client.write_all(fragment).unwrap();
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    client.write_all(&[1, 0]).unwrap();
    expected(&mut client, &[5, 0]);
    client
        .write_all(&connect_packet("website.test", 443))
        .unwrap();
    assert_exchange(client);
    done_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    fake_exit.join().unwrap();
    fake_core.join().unwrap();
    drop(server);
}

#[test]
fn socks5_chain_preserves_order_domains_authentication_and_half_close() {
    for authenticated in [false, true] {
        let exit = listener();
        let exit_addr = exit.local_addr().unwrap();
        let fake_exit = thread::spawn(move || {
            let (mut stream, _) = exit.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let method = if authenticated { 2 } else { 0 };
            expected(&mut stream, &[5, 1, method]);
            // Fragment the greeting across writes.
            stream.write_all(&[5]).unwrap();
            stream.write_all(&[method]).unwrap();
            if authenticated {
                expected(
                    &mut stream,
                    &[1, 4, b'u', b's', b'e', b'r', 4, b'p', b'a', b's', b's'],
                );
                stream.write_all(&[1, 0]).unwrap();
            }
            expected(&mut stream, &connect_packet("website.test", 443));
            exchange(stream);
        });
        let (core, fake_core) = outer(exit_addr);
        let auth = authenticated.then(|| Credentials {
            username: "user".into(),
            password: "pass".into(),
        });
        let (server, addr) = start(core, Kind::Socks5, exit_addr.port(), auth);
        assert_exchange(client_connect(addr));
        fake_exit.join().unwrap();
        fake_core.join().unwrap();
        drop(server);
    }
}

#[test]
fn http_connect_uses_final_proxy_auth_and_keeps_early_bytes() {
    let exit = listener();
    let exit_addr = exit.local_addr().unwrap();
    let fake_exit = thread::spawn(move || {
        let (mut stream, _) = exit.accept().unwrap();
        expected(&mut stream, b"CONNECT website.test:443 HTTP/1.1\r\nHost: website.test:443\r\nProxy-Authorization: Basic dXNlcjpwYXNz\r\n\r\n");
        stream
            .write_all(
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 Connection established\r\n\r\nEARLY",
            )
            .unwrap();
        let mut body = Vec::new();
        stream.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"payload");
        stream.write_all(b"FINAL-EXIT").unwrap();
    });
    let (core, fake_core) = outer(exit_addr);
    let (server, addr) = start(
        core,
        Kind::Http,
        exit_addr.port(),
        Some(Credentials {
            username: "user".into(),
            password: "pass".into(),
        }),
    );
    assert_exchange(client_connect(addr));
    fake_exit.join().unwrap();
    fake_core.join().unwrap();
    drop(server);
}

#[test]
fn rejected_final_authentication_fails_the_client_connection() {
    for kind in [Kind::Http, Kind::Socks5] {
        let exit = listener();
        let exit_addr = exit.local_addr().unwrap();
        let fake_exit = thread::spawn(move || {
            let (mut stream, _) = exit.accept().unwrap();
            if kind == Kind::Http {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(read_array::<1>(&mut stream).unwrap()[0]);
                }
                stream
                    .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                    .unwrap();
            } else {
                expected(&mut stream, &[5, 1, 2]);
                // Do not permit an auth downgrade to an unauthenticated exit.
                stream.write_all(&[5, 0]).unwrap();
            }
        });
        let (core, fake_core) = outer(exit_addr);
        let (server, addr) = start(
            core,
            kind,
            exit_addr.port(),
            Some(Credentials {
                username: "u".into(),
                password: "p".into(),
            }),
        );
        let mut client = client_connect(addr);
        expected(&mut client, &[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(client.read(&mut [0]).unwrap(), 0);
        fake_exit.join().unwrap();
        fake_core.join().unwrap();
        drop(server);
    }
}

#[test]
fn aether_connection_failure_has_no_direct_fallback() {
    let core = listener();
    let core_addr = core.local_addr().unwrap();
    // Keep the port reserved while rejecting the connection at SOCKS level.
    let worker = thread::spawn(move || {
        let (mut stream, _) = core.accept().unwrap();
        expected(&mut stream, &[5, 1, 0]);
        stream.write_all(&[5, 0]).unwrap();
        expected(&mut stream, &connect_packet("exit.test", 1080));
        stream.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
    });
    let (server, addr) = start(core_addr, Kind::Socks5, 1080, None);
    let mut client = client_connect(addr);
    expected(&mut client, &[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    worker.join().unwrap();
    drop(server);
}

#[test]
fn disabled_udp_and_bind_are_rejected_without_contacting_aether() {
    let core = listener();
    core.set_nonblocking(true).unwrap();
    let (server, addr) = start(core.local_addr().unwrap(), Kind::Socks5, 1080, None);
    for cmd in [2, 3] {
        let mut client = socket(addr);
        client.write_all(&[5, 1, 0]).unwrap();
        expected(&mut client, &[5, 0]);
        client.write_all(&[5, cmd, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        expected(&mut client, &[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]);
    }
    assert_eq!(core.accept().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    drop(server);
}

#[test]
fn disconnect_closes_active_streams_and_the_listener() {
    let exit = listener();
    let exit_addr = exit.local_addr().unwrap();
    let (closed_tx, closed_rx) = mpsc::channel();
    let fake_exit = thread::spawn(move || {
        let (mut stream, _) = exit.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        expected(&mut stream, &[5, 1, 0]);
        stream.write_all(&[5, 0]).unwrap();
        expected(&mut stream, &connect_packet("website.test", 443));
        stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        closed_tx.send(()).unwrap();
    });
    let (core, fake_core) = outer(exit_addr);
    let (server, addr) = start(core, Kind::Socks5, exit_addr.port(), None);
    let mut client = client_connect(addr);
    expected(&mut client, &[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    drop(server);
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    closed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(TcpStream::connect(addr).is_err());
    fake_exit.join().unwrap();
    fake_core.join().unwrap();
}

#[test]
fn validation_rejects_url_injection_and_missing_credentials() {
    for host in [
        "http://proxy.example",
        "user:secret@proxy.example",
        "bad\r\nHost: injected",
        "",
    ] {
        assert!(Config {
            udp_enabled: false,
            kind: Kind::Http,
            host: host.into(),
            port: 8080,
            credentials: None
        }
        .validate()
        .is_err());
    }
    assert!(Config {
        udp_enabled: false,
        kind: Kind::Socks5,
        host: "exit.test".into(),
        port: 1080,
        credentials: Some(Credentials {
            username: "user".into(),
            password: String::new()
        })
    }
    .validate()
    .is_err());
    assert!(!format!(
        "{:?}",
        Credentials {
            username: "username-secret".into(),
            password: "password-secret".into()
        }
    )
    .contains("secret"));
}

#[test]
fn ipv6_and_ipv4_target_framing() {
    assert_eq!(
        Target::new("192.0.2.1", 443).unwrap().encode(),
        vec![1, 192, 0, 2, 1, 1, 187]
    );
    let v6 = Target::new("[2001:db8::1]", 443).unwrap();
    assert_eq!(v6.authority(), "[2001:db8::1]:443");
    assert_eq!(
        v6.encode(),
        vec![4, 0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 187]
    );
}

#[test]
fn base64_padding_for_http_basic_auth() {
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("user:pass", "dXNlcjpwYXNz"),
    ] {
        assert_eq!(base64(plain.as_bytes()), encoded);
    }
}

#[test]
fn stopping_during_final_proxy_verification_cancels_the_probe() {
    let core = listener();
    let core_addr = core.local_addr().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let (mut stream, _) = core.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        expected(&mut stream, &[5, 1, 0]);
        entered_tx.send(()).unwrap();
        // A stalled Aether handshake must still be cancelled on Disconnect.
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    let (server, _) = start(core_addr, Kind::Socks5, 1080, None);
    let probe = server.probe();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx.send(probe.verify().is_err()).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    drop(server);
    assert!(done_rx.recv_timeout(Duration::from_secs(3)).unwrap());
    worker.join().unwrap();
}

#[test]
fn total_handshake_deadline_closes_a_slow_connection() {
    let core = listener();
    let (server, _) = start(core.local_addr().unwrap(), Kind::Socks5, 1080, None);
    let pair = listener();
    let mut remote = socket(pair.local_addr().unwrap());
    let (local, _) = pair.accept().unwrap();
    let tracked = server.shared.track(local).unwrap();
    server
        .shared
        .sockets
        .lock()
        .unwrap()
        .get_mut(&tracked.id)
        .unwrap()
        .deadline = Some(Instant::now());
    server.shared.expire_handshakes();
    assert_eq!(remote.read(&mut [0]).unwrap(), 0);
    drop(tracked);
    drop(server);
}
