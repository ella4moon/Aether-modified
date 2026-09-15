# Final proxy support for Aether-GUI

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
6. Click **Connect**. The app checks the final proxy through Aether before it
   displays **Connected via final proxy**.
7. Use the displayed local SOCKS5 address in your application. Its default is
   `127.0.0.1:1819`, the same address the unmodified GUI uses.

The connection order is:

**Application → local GUI proxy → Aether/WireGuard → your remote proxy → website.**

The website receives a connection from your proxy's exit address. A rotating
proxy may use a different exit address for each connection.

## Supported behavior

- TCP connections, including normal HTTPS websites, WebSockets and other TCP apps.
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

- This feature applies to applications using the displayed SOCKS5 listener.
  It does not configure a system-wide proxy or a virtual network adapter.
- UDP ASSOCIATE and SOCKS BIND return "command not supported" while final-proxy
  mode is enabled. UDP and QUIC are not forwarded by this relay.
- The final proxy must be reachable from inside the Aether tunnel. A proxy
  running only on your laptop's `127.0.0.1` is not a reachable remote exit.
- HTTP CONNECT describes the connection to the proxy. HTTPS websites work
  through it, but TLS-wrapped `https://` proxy endpoints, SOCKS4 and other proxy
  protocols are not implemented.
- The startup check asks the final proxy to open `example.com:443`, then closes
  it without sending application data. That destination must be allowed by your
  proxy. This checks both CONNECT hops; it does not measure or attest an exit IP.
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

`src-tauri/exit-proxy` is a small Rust library with no third-party dependencies.
Its only outbound TCP dial is to the Aether loopback listener. It then performs
SOCKS CONNECT to the final proxy and SOCKS CONNECT or HTTP CONNECT to the target.
The Aether process uses an internal loopback port while the GUI relay owns the
usual application-facing port.

The loopback integration tests use mock Aether and final-proxy servers. They
check hop order, remote hostname handling, authentication, coalesced response
bytes, half-close behavior, failures, unsupported UDP, cancellation and deadlines.
They do not contact a real WARP gateway or your private proxy.

```powershell
cargo test --manifest-path src-tauri/exit-proxy/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
npm run build
npm run lint
```

The original project and these modifications are provided under AGPL-3.0-only;
see `LICENSE` and the source files. This is a modified build, not an upstream release.
