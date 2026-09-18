# Whole laptop on Windows 11

Version 0.9.0 adds UDP to **Advanced → Whole laptop (Windows)**. Ordinary Windows apps
can use your final proxy without application proxy settings or a browser
extension. This works with the WireGuard protocol you already use in Aether.

**Windows app → virtual adapter → local relay → Aether/WireGuard → your final proxy → website**

## Setup

1. Open the latest successful [build](https://github.com/ella4moon/Aether-modified/actions/workflows/build.yml)
   on `main` (builds now start automatically on updates; **Run workflow** is also
   available). Extract `bundles-windows-x86_64`, then install the **0.9.1** setup
   executable or MSI. Version 0.9.1 fixes the first-connection adapter check that
   could fail with a blank “Windows network recovery failed” message in 0.9.0.
   Version 0.8.0 does not forward UDP.
2. Quit the running Aether-GUI from its tray menu. Right-click the newly installed
   app and choose **Run as administrator**. Windows requires elevation to create
   the adapter and routes. No permanent service or startup task is installed.
3. In **Advanced**, keep your working **WireGuard** and **Final proxy** settings.
   Re-enter proxy credentials if needed; they stay in memory. Keep **SOCKS5 Proxy**
   on a loopback address such as `127.0.0.1:1819`, with LAN sharing disabled.
4. Choose a **SOCKS5** endpoint that supports **UDP ASSOCIATE**, and enable
   **Final proxy → Forward UDP**. This setting also enables UDP for applications
   using the local SOCKS5 listener directly. HTTP CONNECT does not carry UDP.
5. Enable **Whole laptop (Windows) → Route apps through my final proxy**. Connect
   and wait for **Whole laptop via final proxy**. Aether and the final proxy are
   checked before Windows traffic capture starts.
6. Turn off FoxyProxy and other application proxy overrides for a clean test.
   Stop other VPN/TUN apps and restart applications with existing connections.
   No Windows manual proxy setting is needed.

In PowerShell, check the public IPv4 address using a new **TCP** connection
without a per-app proxy (this HTTPS check does not verify UDP):

```powershell
curl.exe -4 --noproxy "*" https://api.ipify.org
```

For a static proxy, the result should be your final proxy's exit address. A
rotating proxy can choose a different exit for each request. An application
configured to use another remote proxy adds another hop; remove that override
if you want your Aether final proxy to be what the destination sees.

## Traffic support

| Traffic | While whole-laptop mode is connected |
| --- | --- |
| Internet TCP, including ordinary HTTPS | Through Aether and your final proxy |
| DNS on TCP/UDP port 53 | DNS-over-HTTPS through the same proxy chain |
| Other internet UDP, including QUIC and many games/voice protocols | Through Aether and your SOCKS5 final proxy when Forward UDP is on; rejected when off |
| Internet ICMP, including ping | Rejected |
| IPv4 and IPv6 | Both captured; the proxy must support the destination |
| Loopback, LAN and Windows link traffic | Remain local according to Windows routes |
| Aether's own transport | Uses the underlying network to carry the tunnel |

UDP uses native SOCKS5 UDP ASSOCIATE at both hops. Your final proxy must allow
the destination and UDP port, and its advertised UDP relay must be reachable
through Aether. The connection check verifies association support; acceptance
alone does not prove delivery to every destination. UDP-dependent apps can
still have service, NAT or proxy restrictions. SOCKS5 fragmentation and datagrams
too large for the additional proxy headers are not supported.

DNS-over-HTTPS continues to use Cloudflare's `1.1.1.1` service over your final
proxy, even with UDP enabled. The Advanced DNS field still configures Aether's
internal tunnel DNS. HTTP final proxies remain usable with Forward UDP off;
whole-laptop mode then carries TCP and DNS and rejects other UDP.

This is Windows routing for ordinary internet connections. More specific routes,
other VPNs, and apps bound to a particular interface can affect which traffic
Windows delivers to the adapter. Local services are not sent to the remote proxy.

## Disconnect and recovery

Click Disconnect before changing modes. Closing Aether-GUI also disconnects,
unless **Close to tray** is enabled; choose **Quit** in the tray menu to exit fully.

**This is not a kill switch.** Capture is active only while connected. During
initial connection, disconnect, automatic reconnection, and after crash cleanup,
apps can use normal Windows networking. If your final proxy stops accepting
connections while capture remains active, captured connections fail instead
of choosing a direct outbound.

A separate helper watches the GUI's lifetime pipe and attempts cleanup even
after a GUI crash. A Windows job object kills remaining children if the helper
itself terminates. If networking appears stuck:

1. Quit other Aether-GUI instances and wait a few seconds.
2. Open Aether-GUI **as administrator**.
3. While disconnected, open **Advanced → Whole laptop (Windows)** and click
   **Restore normal networking**. Recovery only removes routes/disables
   `AetherWholeLaptop` when the app has its ownership record. It leaves Wi-Fi,
   Ethernet, Windows proxy settings, and their configured DNS servers unchanged.
4. If Windows still has stale driver/network state, restart Windows.

The app data directory holds local adapter configuration and a recovery record.
These contain no remote proxy credentials. A disabled adapter left after a
forced stop is reused only if this app still has its ownership record.

## Build and implementation

The Windows workflow and `build-windows.ps1` download the unmodified, checksum
verified sing-box **1.14.1** archive. Its executable, companion DLL, license and
[source attribution](src-tauri/binaries/THIRD-PARTY.md) are included in the installer.
There is no runtime download. macOS/Linux retain the existing per-app mode.

For manual Windows development, run
`./src-tauri/binaries/fetch-system-tunnel.ps1` before `npm run tauri dev` or a
Tauri build, in addition to fetching Aether. Elevate the development shell only
when testing the virtual adapter.

`src-tauri/whole-laptop` owns configuration, elevation checks, process supervision
and recovery. The helper invokes the GUI executable before Tauri starts. Only
the exact bundled Aether executable has a direct transport route. Other apps
are not generally excluded. Ordinary TCP, enabled UDP, and the DNS-over-HTTPS server
explicitly use the SOCKS outbound pointing to the existing loopback relay.

The workflow checks the generated configuration with the **bundled Windows
executable** before producing installers. See [VERIFICATION.md](VERIFICATION.md)
for local test results and outstanding Windows runtime checks.
