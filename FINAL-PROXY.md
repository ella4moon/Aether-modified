# Final proxy support for Aether-GUI

For apps without proxy settings on Windows 11, see the new
[Whole laptop option](WHOLE-LAPTOP.md). Version **0.9.0** adds native UDP support
to both the local relay and whole-laptop mode.

This modified source is published at [ella4moon/Aether-modified](https://github.com/ella4moon/Aether-modified)
and is based on [MatinSenPai/Aether-GUI](https://github.com/MatinSenPai/Aether-GUI) commit
`93314fcd97bf6b446d537aac9538b01bef04c7a0` (version 0.7.0).
It keeps the upstream Aether engine pinned to v1.5.0 and adds an optional final
proxy in the GUI. WireGuard is supported with the same engine and settings.

## Use it

1. Build this modified GUI using the Windows instructions below, or its existing
   GitHub Actions `build` workflow. The source archive is not an installer.
2. Open **Advanced**, choose **WireGuard**, and keep your working connection settings.
3. Under **Final proxy**, turn on **Use a final proxy**.
4. Choose **SOCKS5** or **HTTP CONNECT** and enter the proxy's host and port separately.
   For example, enter `proxy.example.com` in Host and `1080` in Port. Use the
   values from your actual proxy service; the example is a placeholder.
5. If needed, turn on **Username and password** and enter both locally in the app.
   Credentials are kept in memory and must be entered again after restarting.
6. To forward UDP, choose **SOCKS5** and turn on **Forward UDP**. Your provider
   must support **UDP ASSOCIATE** at that endpoint; SOCKS5 support alone does not
   guarantee this. HTTP CONNECT can still be used for TCP.
7. Click **Connect**. The app checks the final proxy through Aether before it
   displays **Connected via final proxy**.
8. Use the displayed local SOCKS5 address in your application. Its default is
   `127.0.0.1:1819`, the same address the unmodified GUI uses.

The connection order is:

**Application → local GUI proxy → Aether/WireGuard → your remote proxy → website.**

The website receives a connection from your proxy's exit address. A rotating
proxy may use a different exit address for each connection.

## Supported behavior

- TCP connections, including normal HTTPS websites, WebSockets and other TCP apps.
- Native UDP datagrams with **Forward UDP** enabled, including UDP applications
  and QUIC where the final proxy and destination permit them. The local app must
  use SOCKS5 UDP ASSOCIATE, or use whole-laptop mode on Windows.
- SOCKS5 final proxies with no authentication or username/password authentication.
- HTTP CONNECT final proxies with no authentication or HTTP Basic authentication.
- Hostnames, IPv4 and IPv6 destinations. Names are passed to the proxies;
  the relay does not resolve website or final-proxy hostnames on the local network.
- Aether resolves the final proxy's hostname through its tunnel. The final proxy
  resolves website hostnames when the application supplies a hostname over SOCKS5.
- A failed final-proxy connection returns an error to the application. There is
  no alternate connection through WARP alone or the computer's ordinary interface.
- Disconnect closes the relay listener, active connections and an in-progress
  final-proxy check. Automatic Aether restarts create a new relay for that session.

## Limits that affect configuration

- This relay serves apps using the displayed SOCKS5 listener. The optional
  [Whole laptop mode](WHOLE-LAPTOP.md) supplies Windows virtual-adapter capture
  for apps without SOCKS support.
- **Forward UDP** is off on upgrade so existing TCP-only proxy setups still work.
  Enable it to use UDP in either mode. A rejected UDP association fails the
  connection check with an actionable error; no direct UDP path is selected.
- ICMP/ping and SOCKS BIND remain unsupported. Ordinary HTTP CONNECT supports
  TCP only. Fragmented SOCKS5 UDP frames, and oversized datagrams that cannot
  fit both SOCKS headers, are dropped.
- The final proxy must be reachable from inside the Aether tunnel. A proxy
  running only on your laptop's `127.0.0.1` is not a reachable remote exit.
- HTTP CONNECT describes the connection to the proxy. HTTPS websites work
  through it, but TLS-wrapped `https://` proxy endpoints, SOCKS4 and other proxy
  protocols are not implemented.
- The startup check asks the final proxy to open `example.com:443`, then closes
  it without sending application data. With **Forward UDP** enabled, it also
  checks UDP ASSOCIATE on both Aether and the final proxy. This confirms the
  control path, not delivery to every UDP port or a measured exit IP. A provider
  can accept associations yet block particular destinations or UDP ports.
- Aether destination routing lists and the organization Gateway proxy are
  inactive in this mode. The core carries the connection to your final proxy;
  destination policy should be configured at that proxy. These saved settings
  are retained for sessions where final-proxy mode is disabled.
- The local listener uses Aether-GUI's existing bind setting and has no
  authentication. Its default is loopback. The relay allows up to 128 simultaneous
  client connections and applies a 15-second total handshake deadline.

## Build on Windows

Install Node.js 22.12 or newer, the stable Rust MSVC toolchain, Microsoft's C++
Build Tools (Desktop development with C++), and WebView2 Runtime as required by
[Tauri's Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows).

In PowerShell, from this extracted project's folder:

```powershell
.\build-windows.ps1
```

The script checks the command-line tools, downloads the pinned upstream Aether Windows
archive and verifies its published SHA-256 checksum, installs the project's
locked npm dependencies, runs tests, and builds the GUI installers.
It does not start a tunnel or ask for your proxy credentials.

Installers are generated under `src-tauri/target/release/bundle/`.

Alternatively, open this repository's [build workflow](https://github.com/ella4moon/Aether-modified/actions/workflows/build.yml),
select **Run workflow**, choose `main`, and run it. After the Windows job succeeds,
open the completed run and download `bundles-windows-x86_64` from **Artifacts**.
Extract the archive and run the included setup executable or MSI installer.
Proxy credentials are not needed for builds or tests.

## Implementation and verification

`src-tauri/exit-proxy` uses Rust and Mio for portable socket readiness.
Its only outbound TCP dial is to the Aether loopback listener. It then performs
SOCKS CONNECT to the final proxy and SOCKS CONNECT or HTTP CONNECT to the target.
The Aether process uses an internal loopback port while the GUI relay owns the
usual application-facing port.

For UDP it keeps two SOCKS5 control connections alive: one to the final proxy
through Aether, and a UDP association with Aether itself. Each client datagram
gets a second SOCKS5 UDP envelope addressed to the final proxy's UDP relay, then
is sent **only to Aether's loopback UDP relay**. Aether carries that UDP packet
through its tunnel, and the final proxy forwards the inner packet. Replies
unwrap in reverse order. Data remains datagrams; TCP carries authentication and
association control. Closing any control connection or Disconnect ends the
association. The GUI never sends UDP directly to the remote proxy or destination.

This uses the existing [Aether 1.5.0 UDP implementation](https://github.com/CluvexStudio/Aether/blob/v1.5.0/aether/src/socks.rs).
WireGuard's tunnel transport was already UDP-capable; the earlier limitation was
in the GUI's final-proxy relay, which originally implemented TCP CONNECT only.

The loopback integration tests use mock Aether and final-proxy servers. They
check TCP and UDP hop order, remote hostname handling, authentication, coalesced
response bytes, half-close behavior, datagram
boundaries, invalid source/fragment rejection, cancellation and deadlines.
They do not contact a real WARP gateway or your private proxy.

```powershell
cargo test --manifest-path src-tauri/exit-proxy/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
npm run build
npm run lint
```

The original project and these modifications are provided under AGPL-3.0-only;
see `LICENSE` and the source files. This is a modified build, not an upstream release.
