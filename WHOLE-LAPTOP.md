# Whole laptop on Windows 11

Version 0.8.0 adds **Advanced → Whole laptop (Windows)**. Ordinary Windows apps
can use your final proxy without application proxy settings or a browser
extension. This works with the WireGuard protocol you already use in Aether.

**Windows app → virtual adapter → local relay → Aether/WireGuard → your final proxy → website**

## Setup

1. Build a fresh installer from `main` using [Run workflow](https://github.com/ella4moon/Aether-modified/actions/workflows/build.yml).
   Extract `bundles-windows-x86_64`, then install the **0.8.0** setup executable
   or MSI. Old installers do not include the virtual adapter component.
2. Quit the running Aether-GUI from its tray menu. Right-click the newly installed
   app and choose **Run as administrator**. Windows requires elevation to create
   the adapter and routes. No permanent service or startup task is installed.
3. In **Advanced**, keep your working **WireGuard** and **Final proxy** settings.
   Re-enter proxy credentials if needed; they stay in memory. Keep **SOCKS5 Proxy**
   on a loopback address such as `127.0.0.1:1819`, with LAN sharing disabled.
4. Enable **Whole laptop (Windows) → Route apps through my final proxy**. Connect
   and wait for **Whole laptop via final proxy**. Aether and the final proxy are
   checked before Windows traffic capture starts.
5. Turn off FoxyProxy and other application proxy overrides for a clean test.
   Stop other VPN/TUN apps and restart applications with existing connections.
   No Windows manual proxy setting is needed.

In PowerShell, check the public IPv4 address using a new connection without a
per-app proxy:

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
| Other internet UDP, including QUIC and many games/voice protocols | Rejected; no direct fallback in the capture policy |
| Internet ICMP, including ping | Rejected |
| IPv4 and IPv6 | Both captured; the proxy must support the destination |
| Loopback, LAN and Windows link traffic | Remain local according to Windows routes |
| Aether's own transport | Uses the underlying network to carry the tunnel |

The final-proxy relay supports **TCP CONNECT**, so arbitrary UDP forwarding is
not available, even with a SOCKS5 final proxy. Browsers usually fall back from
QUIC to TCP HTTPS; apps that require UDP may not work. DNS-over-HTTPS uses
Cloudflare's `1.1.1.1` service over your final proxy. The Advanced DNS field still
configures Aether's internal tunnel DNS.

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
are not generally excluded. Both ordinary TCP and the DNS-over-HTTPS server
explicitly use the SOCKS outbound pointing to the existing loopback relay.

The workflow checks the generated configuration with the **bundled Windows
executable** before producing installers. See [VERIFICATION.md](VERIFICATION.md)
for local test results and outstanding Windows runtime checks.
