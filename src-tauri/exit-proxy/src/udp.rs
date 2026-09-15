//! Two SOCKS5 UDP layers, not a UDP-over-TCP stream:
//! local client frame -> Aether frame addressed to final relay -> final proxy.
//! TCP is used only to authenticate and keep the UDP associations alive.
use super::*;
use mio::{Events, Interest, Poll, Token};
use std::net::{Ipv4Addr, Ipv6Addr, UdpSocket};

const MAX_DATAGRAM: usize = 65_507;
const BATCH: usize = 64;

fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}

fn context(error: io::Error, message: &str) -> io::Error {
    io::Error::new(error.kind(), format!("{message}: {error}"))
}

/// Decode only one complete RFC 1928 UDP frame. Fragmentation is optional in
/// SOCKS5 and is not implemented: malformed/fragmented packets are discarded.
fn decode(packet: &[u8]) -> Option<(Target, &[u8])> {
    if packet.get(..3)? != [0, 0, 0] {
        return None;
    }
    let (host, end) = match *packet.get(3)? {
        1 => (
            Ipv4Addr::from(<[u8; 4]>::try_from(packet.get(4..8)?).ok()?).to_string(),
            8,
        ),
        4 => (
            Ipv6Addr::from(<[u8; 16]>::try_from(packet.get(4..20)?).ok()?).to_string(),
            20,
        ),
        3 => {
            let end = 5 + *packet.get(4)? as usize;
            (
                std::str::from_utf8(packet.get(5..end)?).ok()?.to_owned(),
                end,
            )
        }
        _ => return None,
    };
    let port = u16::from_be_bytes(packet.get(end..end + 2)?.try_into().ok()?);
    let target = Target::new(&host, port).ok()?;
    if target.host != host {
        return None;
    }
    Some((target, packet.get(end + 2..)?))
}

fn prefix(target: &Target) -> Vec<u8> {
    let mut packet = vec![0, 0, 0];
    packet.extend(target.encode());
    packet
}

fn local_relay(bound: Target, core: SocketAddr) -> io::Result<SocketAddr> {
    let ip = bound
        .host
        .parse::<IpAddr>()
        .map_err(|_| invalid("Aether's UDP relay must return a loopback IP address"))?;
    let ip = if ip.is_unspecified() { core.ip() } else { ip };
    if !ip.is_loopback() || normalize(ip) != normalize(core.ip()) || bound.port == 0 {
        return Err(invalid(
            "Aether's UDP relay must use its configured loopback interface and a nonzero port",
        ));
    }
    Ok(SocketAddr::new(ip, bound.port))
}

pub(super) struct Association {
    final_control: Tracked,
    core_control: Tracked,
    tunnel: mio::net::UdpSocket,
    final_relay: Target,
}

impl Association {
    pub(super) fn open(shared: &Arc<Shared>) -> io::Result<Self> {
        if !shared.config.udp_enabled || shared.config.kind != Kind::Socks5 {
            return Err(invalid(
                "UDP requires an enabled SOCKS5 final proxy with UDP ASSOCIATE support",
            ));
        }
        // This TCP connection is itself a CONNECT through Aether. The final
        // server sees the tunnel's egress as the UDP association's client.
        let mut final_control = shared.connect_final()?;
        socks_login(
            &mut final_control.stream,
            shared.config.credentials.as_ref(),
        )?;
        let unspecified = Target {
            host: "0.0.0.0".into(),
            port: 0,
        };
        let mut final_relay = socks_request(&mut final_control.stream, 3, &unspecified)
            .map_err(|e| context(e, "Final proxy UDP association failed"))?;
        // Some servers return an unspecified address to mean their own host.
        // Keep a hostname encoded for Aether to resolve through its tunnel.
        if final_relay
            .host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified())
        {
            final_relay.host = shared.config.host.clone();
        }
        final_relay = Target::new(&final_relay.host, final_relay.port)?;

        let tunnel = UdpSocket::bind(SocketAddr::new(shared.core.ip(), 0))?;
        let source = tunnel.local_addr()?;
        let mut core_control = shared.connect_core()?;
        let bound = socks_request(
            &mut core_control.stream,
            3,
            &Target {
                host: source.ip().to_string(),
                port: source.port(),
            },
        )
        .map_err(|e| context(e, "Aether UDP association failed"))?;
        // This is the ONLY UDP peer contacted by the production relay. An
        // unexpected BND.ADDR cannot make us send directly to the final proxy.
        tunnel.connect(local_relay(bound, shared.core)?)?;
        tunnel.set_nonblocking(true)?;
        final_control.handshake_complete()?;
        core_control.handshake_complete()?;
        Ok(Self {
            final_control,
            core_control,
            tunnel: mio::net::UdpSocket::from_std(tunnel),
            final_relay,
        })
    }

    fn expected_relay(&self, actual: &Target) -> bool {
        if actual.port != self.final_relay.port {
            return false;
        }
        match (
            self.final_relay.host.parse::<IpAddr>(),
            actual.host.parse::<IpAddr>(),
        ) {
            (Ok(expected), Ok(actual)) => normalize(expected) == normalize(actual),
            (Err(_), Ok(_)) => {
                // Aether resolves domain-form relay addresses remotely and
                // returns the resolved source IP. Do not introduce local DNS.
                // The connected UDP socket already accepts only Aether's peer.
                true
            }
            (Err(_), Err(_)) => self.final_relay.host.eq_ignore_ascii_case(&actual.host),
            _ => false,
        }
    }

    fn run(mut self, client: Tracked, requested: Target, shared: &Arc<Shared>) -> io::Result<()> {
        let peer_ip = normalize(client.stream.peer_addr()?.ip());
        let relay = UdpSocket::bind(SocketAddr::new(client.stream.local_addr()?.ip(), 0))?;
        let relay_addr = relay.local_addr()?;
        relay.set_nonblocking(true)?;
        let mut relay = mio::net::UdpSocket::from_std(relay);
        let mut poll = Poll::new()?;
        poll.registry()
            .register(&mut relay, Token(0), Interest::READABLE)?;
        poll.registry()
            .register(&mut self.tunnel, Token(1), Interest::READABLE)?;
        let mut controls = Vec::new();
        for (i, tracked) in [&client, &self.final_control, &self.core_control]
            .into_iter()
            .enumerate()
        {
            tracked.stream.set_nonblocking(true)?;
            let mut socket = mio::net::TcpStream::from_std(tracked.stream.try_clone()?);
            poll.registry()
                .register(&mut socket, Token(i + 2), Interest::READABLE)?;
            controls.push(socket);
        }
        // Use Mio for every I/O operation once registered (required by IOCP).
        let mut response = vec![5, 0, 0];
        response.extend(
            Target {
                host: relay_addr.ip().to_string(),
                port: relay_addr.port(),
            }
            .encode(),
        );
        // A small SOCKS response can still encounter WouldBlock. Register write
        // readiness temporarily so partial replies never fail spuriously.
        poll.registry().reregister(
            &mut controls[0],
            Token(2),
            Interest::READABLE | Interest::WRITABLE,
        )?;
        let mut response_pos = 0;
        let mut events = Events::with_capacity(16);
        let mut readable = [false; 2];
        let mut latched = None;
        let outer_prefix = prefix(&self.final_relay);
        let mut input = vec![0; 65_535];
        let mut wrapped = Vec::with_capacity(65_535);
        while !shared.stopped.load(Ordering::Acquire) {
            let timeout = if readable.iter().any(|v| *v) {
                Duration::ZERO
            } else {
                Duration::from_millis(100)
            };
            match poll.poll(&mut events, Some(timeout)) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                other => other?,
            }
            for event in events.iter() {
                let token = event.token().0;
                if token < 2 {
                    readable[token] = true;
                    continue;
                }
                let control = &mut controls[token - 2];
                if token == 2 && response_pos < response.len() && event.is_writable() {
                    while response_pos < response.len() {
                        match control.write(&response[response_pos..]) {
                            Ok(0) => return Ok(()),
                            Ok(written) => response_pos += written,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                            Err(error) => return Err(error),
                        }
                    }
                    if response_pos == response.len() {
                        poll.registry()
                            .reregister(control, Token(2), Interest::READABLE)?;
                        client.handshake_complete()?;
                    }
                }
                if event.is_readable() {
                    match control.read(&mut [0u8; 1]) {
                        Ok(_) => return Ok(()), // EOF or unexpected control data ends association.
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => return Ok(()),
                    }
                }
            }
            for (direction, ready) in readable.iter_mut().enumerate() {
                if !*ready {
                    continue;
                }
                // Retain readiness after a batch, yielding to control events
                // and the other direction. Drain until WouldBlock before sleep.
                for _ in 0..BATCH {
                    if shared.stopped.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let received = if direction == 0 {
                        relay.recv_from(&mut input)
                    } else {
                        self.tunnel.recv(&mut input).map(|n| (n, relay_addr))
                    };
                    let (n, from) = match received {
                        Ok(value) => value,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            *ready = false;
                            break;
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    };
                    if direction == 0 {
                        if response_pos < response.len()
                            || normalize(from.ip()) != peer_ip
                            || (requested.port != 0 && from.port() != requested.port)
                            || latched.is_some_and(|known| known != from)
                            || decode(&input[..n]).is_none()
                            || n + outer_prefix.len() > MAX_DATAGRAM
                        {
                            continue;
                        }
                        latched = Some(from);
                        wrapped.clear();
                        wrapped.extend_from_slice(&outer_prefix);
                        wrapped.extend_from_slice(&input[..n]);
                        // UDP has no delivery guarantee: drop a datagram on
                        // queue pressure, preserving boundaries and responsiveness.
                        match self.tunnel.send(&wrapped) {
                            Ok(_) => {}
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Err(error) => return Err(error),
                        }
                    } else if let Some(client_addr) = latched {
                        let Some((source, inner)) = decode(&input[..n]) else {
                            continue;
                        };
                        if !self.expected_relay(&source) || decode(inner).is_none() {
                            continue;
                        }
                        match relay.send_to(inner, client_addr) {
                            Ok(_) => {}
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub(super) fn handle(
    mut client: Tracked,
    shared: &Arc<Shared>,
    requested: Target,
) -> io::Result<()> {
    let peer = normalize(client.stream.peer_addr()?.ip());
    let declared = requested
        .host
        .parse::<IpAddr>()
        .map_err(|_| invalid("UDP client source must be an IP address"));
    if !declared.is_ok_and(|ip| ip.is_unspecified() || normalize(ip) == peer) {
        reply(&mut client.stream, 2)?;
        return Err(refused(
            "UDP source must match the SOCKS5 TCP client's address",
        ));
    }
    let association = match Association::open(shared) {
        Ok(value) => value,
        Err(error) => {
            let _ = reply(&mut client.stream, 1);
            return Err(error);
        }
    };
    association.run(client, requested, shared)
}

#[cfg(test)]
mod tests;
