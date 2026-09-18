# Verification of the final-proxy changes

## Windows returned-packet checks, version 0.9.3 (2026-09-18)

The new laptop log confirms successful route selection and captured DNS, while
ordinary HTTPS connections time out. It does not establish whether the failed
TCP connections reached the Windows forwarding listener or whether the proxy
chain survived capture. The earlier /32 CI test did not use native DNS or
strict-route filters and did not verify the full chain on the user's laptop.

The bundled sing-tun's [system TCP stack](https://github.com/SagerNet/sing-tun/blob/v0.9.3/stack_system.go)
creates a Windows TCP listener and ignores errors from its firewall setup.
Version 0.9.3 uses gVisor for TCP as well as UDP, removing that dependency.
The precise cause on the laptop remains unconfirmed; a firewall block is a
possible failure mode, not an observed fact from the supplied logs.

Before reporting connected, the helper now requires:

1. The existing Windows route-selection checks.
2. A unique TCP payload echoed through the real TUN into a loopback-only probe.
   A SYN/accept or an installed route alone cannot pass this check.
3. A successful DNS answer for example.com through the TUN DNS listener. In
   production this requires DNS-over-HTTPS through Aether and the final proxy
   after the capture routes are installed. TCP-only mode also supports this.

Failures stop capture and report the failing stage. The local TCP probe uses
only one IANA benchmark /32 and ephemeral port, with a destination override
to loopback. It never introduces a general direct Internet route.

The Windows CI gate now runs those exact Rust packet checks as well as ordinary
TCP and UDP echo traffic. It enables the real TUN's native DNS and strict-route
filters. The upstream DNS mock uses DNS/TCP through SOCKS, so this does not test
the user's private proxy or public TLS/DoH service. Only benchmark routes are
installed; cleanup stops the TUN, disables its unique adapter and clears the
DNS cache. A readiness check cannot prove every later website is reachable.

Cross-platform tests also cover a returned TCP payload, bounded connection
failure, the probe's narrow routing scope, and rejecting invalid DNS replies.
The workflow must pass before producing the 0.9.3 Windows installer.

## Windows capture and readiness, version 0.9.2 (2026-09-18)

A report showed a green whole-laptop status while a fresh-looking curl result
did not match the configured final proxy address. The supplied log contained
only Aether-core output, so it did not establish the laptop's selected routes
or the proxy's actual exit address. Earlier readiness checks only observed
`sing-box started` and could not detect Windows choosing another interface.

Version 0.9.2 configures `0.0.0.0/1`, `128.0.0.0/1`, `::/1`, and `8000::/1`
on the owned TUN adapter, giving these routes precedence over competing default
routes without changing the user's other interfaces. Before announcing ready,
the helper checks the adapter, the four routes, and Windows' actual selections
for representative IPv4/IPv6 destinations. Missing routes or a conflicting
selected interface abort startup with a diagnostic. Startup/initial traffic
and rate-limited problems from sing-box are now included in the GUI log.

The Windows test suite checks successful route verification, a competing
selected interface, and a missing capture route. A separate CI integration
gate runs the bundled sing-box against a loopback SOCKS5 mock using a real
Windows TUN adapter. Ordinary TCP and UDP sockets send unique payloads to a
benchmark address; the test requires the mock SOCKS outbound to observe and
echo both payloads. It uses one /32 route and disables native DNS/WFP changes
for the test adapter, preserving the runner's default connectivity. The TUN
stack, SOCKS outbound and automatic outbound-interface binding are exercised.

This packet test does not establish behavior on the user's Windows 11 laptop,
verify the user's private proxy, or test native DNS hijacking. The startup
route checks are point-in-time checks, not a kill switch or proof that every
destination is reachable. The Windows build must pass this new gate before an
installer is produced.

## Windows adapter discovery, version 0.9.1 (2026-09-18)

The reported blank `Windows network recovery failed:` error can occur before
the TUN adapter is created. The old startup query used `Get-NetAdapter -Name`
with `-ErrorAction SilentlyContinue`. An absent adapter is expected on first
connection, but that cmdlet reports it as an error; suppressing the message
does not make `powershell.exe -Command` return success.

Discovery and cleanup now enumerate adapters and filter by the exact owned
name. An empty result is accepted, while real enumeration errors stop the
operation. PowerShell failures include the operation, exit status and available
output. Redirected output uses UTF-8; bytes emitted before the encoding setting
are decoded lossily rather than dropping the entire diagnostic.

The Windows whole-laptop tests now exercise:

- The real Windows PowerShell/NetAdapter lookup for an absent adapter.
- Exact-name filtering with unrelated and similarly named adapters.
- A genuine enumeration failure, which must remain an error.
- A failed command with no output, which must still give a useful message.
- Localized error text through redirected PowerShell output.

The tests are part of the existing native Windows build gate. They do not
create adapters or change routes. They verify the startup query and diagnostics;
successful traffic capture on a particular Windows 11 laptop still needs a
runtime check after installing the new build.

## Native UDP and whole-laptop UDP, version 0.9.0 (2026-09-15)

| Local check | Result |
| --- | --- |
| TypeScript/Vite production build and ESLint | Passed |
| Linux desktop backend tests | 23 passed, including upgrade defaults and saving the UDP preference |
| Final-proxy relay tests | 21 passed, including nine UDP tests |
| Whole-laptop policy tests | 5 passed, covering both TCP-only and TCP/UDP configurations |
| Windows x64 relay library and tests, Clippy with warnings denied | Passed using the Windows MSVC target |
| Windows x64 whole-laptop library and tests, Clippy with warnings denied | Passed using the Windows MSVC target |
| Both generated configurations checked by pinned sing-box 1.14.1 on Linux | Passed; no adapter started |
| Git whitespace check | Passed |

The UDP integration tests use actual loopback UDP sockets for separate mock
Aether and final-proxy hops. They check nested SOCKS5 envelopes, authentication,
domain-form relay/destination addresses, empty and larger datagrams, source-port
pinning, malformed/fragmented packet rejection, association refusal, and socket
cleanup when the client disconnects, either upstream control connection closes,
or the server stops. A separate test checks cancellation during an incomplete
UDP handshake. HTTP CONNECT cannot be configured as UDP-capable.

UDP payloads stay in UDP datagrams. The TCP connections carry SOCKS5
authentication and association control only. The relay's outbound UDP socket
accepts only the verified Aether loopback peer; neither the final proxy nor the
destination is dialled directly by the GUI. Aether's existing WireGuard transport
and UDP implementation are unchanged.

The startup probe verifies TCP CONNECT and both UDP ASSOCIATE handshakes. It
does **not** establish that a provider permits every UDP destination or demonstrate
a live UDP exit IP. These local checks do not use a real Aether tunnel, the user's
private proxy, or a Windows 11 TUN adapter. Native Windows runtime testing of
installation, adapter creation, forwarding, disconnect and recovery remains
necessary. See [WHOLE-LAPTOP.md](WHOLE-LAPTOP.md).

The build workflow now starts on `main` pushes. It runs the relay and helper
tests on native runners and checks both generated configurations with the
bundled Windows sing-box before building installers. The older results below
describe earlier versions; version 0.9.0 enables UDP when **Forward UDP** is on.

### Native build results and Windows assertion correction (2026-09-16)

The first [0.9.0 native run](https://github.com/ella4moon/Aether-modified/actions/runs/35008019017)
passed all build and test steps on Linux and both macOS runners. Windows passed
20 of 21 relay tests, including UDP forwarding, authentication, source validation
and shutdown. Its rejection test expected EOF but received Winsock
`ConnectionReset` on a closed upstream association. The assertion now accepts
either close signal; timeouts, received data and any UDP packets after refusal
still fail the test. Use a subsequent successful build of this correction for
the Windows installer.

The [next Windows run](https://github.com/ella4moon/Aether-modified/actions/runs/35058247088)
passed all 21 relay tests and all 23 backend tests. It then exposed an early
helper exit in both subprocess-supervision tests, before readiness was reported.
The helper now starts with `DETACHED_PROCESS`, the documented partner for
[AllocConsole](https://learn.microsoft.com/en-us/windows/console/allocconsole),
in both production and the test fixture. Child startup errors are visible in CI,
and helper tests run before the desktop backend build. Route/adapter cleanup
requirements are unchanged.

## Windows whole-laptop option, version 0.8.0 (2026-09-15)

| Local check | Result |
| --- | --- |
| TypeScript/Vite production build and ESLint | Passed |
| Linux desktop backend compile and unit tests | Passed; 22 tests, including old-profile capture defaulting to off |
| Existing final-proxy relay suite | 12 tests passed |
| Whole-laptop routing policy suite | 4 tests passed |
| Windows x64 whole-laptop library and test compilation, Clippy with warnings denied | Passed using the Windows MSVC target |
| Generated config checked by pinned sing-box 1.14.1 on Linux | Passed; no adapter started |
| Windows distribution archive SHA-256 | Matched the pinned digest |
| Git whitespace check | Passed |

The new policy tests check both IP families, loopback/final-proxy prerequisites,
the DNS-over-HTTPS detour through the local relay, rejection of other UDP/ICMP,
and exact executable-path matching for Aether's transport exception (including
case differences, the Windows extended-path prefix, and regex metacharacters).
Existing relay tests still verify the order of the Aether and final proxy hops,
authentication, socket shutdown and the absence of a direct fallback.

Windows-only subprocess tests were added for lifetime-pipe EOF and forced helper
termination. They exercise the actual hidden-console and Windows job-object code
without changing routes. Their two subprocess fixtures are marked ignored to
prevent the test harness from invoking them directly; the parent tests invoke
them explicitly. These tests were **compiled**, not executed, in this Linux
environment. A fresh Windows workflow run executes them and checks the generated
config with the bundled Windows `sing-box.exe` before creating installers.

The Windows installer, administrator check, real Wintun adapter, live DNS and
traffic forwarding, recovery button, sleep/resume, and network changes still
need a **native Windows 11 test**. No claim is made here of a Windows end-to-end
run or observed exit IP. Use the [setup and recovery instructions](WHOLE-LAPTOP.md),
test a fresh connection with application proxy overrides disabled, and confirm
that Disconnect restores networking. Whole-laptop mode is off by default and
is not a kill switch; initial connection, reconnect and post-crash cleanup restore
ordinary networking. UDP-dependent applications are outside this TCP relay's
capabilities.

The Linux backend tests again used the isolated system-library files and one
test code-generation unit for GTK. The existing Linux-only unused-import warning
in `src/focus.rs` remains; the Windows helper has no Clippy warnings.

## Platform socket fix (2026-09-15)

The first [GitHub Actions run](https://github.com/ella4moon/Aether-modified/actions/runs/34933354701)
built source commit `e88b8ddd81ae2de7bf0226d28e33e9297b1574c5` with these results:

| Platform | Observed result |
| --- | --- |
| Linux x64 | Relay and backend tests passed; installers built |
| macOS ARM64 and x64 | Six relay tests failed; packaging did not run |
| Windows x64 | Four relay tests failed, but the combined PowerShell step masked the failure and produced installers |

The old Windows artifact is not a fully validated build despite its green job
status. Use a fresh build of the corrected source described below.

Accepted sockets can inherit nonblocking mode from the listener on
[macOS](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/accept.2.html)
and [Windows](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-accept).
The relay uses blocking reads in dedicated client threads, so an inherited
nonblocking socket could terminate a handshake when the next bytes had not
arrived yet. The handler now explicitly selects blocking mode before reading.
Existing timeouts and Disconnect cancellation still apply.

Each frontend and Rust validation command now has a separate workflow step.
This prevents a successful later command from hiding a failed test through
[PowerShell's last-command exit status](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#exit-codes-and-error-action-preference).

Local validation of the correction:

- A new regression test fails against the original handler and passes with
  the correction. It starts with a nonblocking accepted socket, delays and
  fragments the greeting, then checks both proxy hops, payload and half-close.
- Emulating inherited nonblocking sockets on Linux reproduced all six macOS
  test failures before the correction. All 12 relay tests pass after it.
- All 12 relay tests also pass with normal Linux socket behavior.
- Relay formatting, Clippy with warnings denied, and Git whitespace checks pass.

The emulation used a temporary Linux test shim for `accept`/`accept4`; it is not
part of the application. A fresh native Windows/macOS workflow run is still
required to validate the corrected platform builds. Start **Run workflow** on
`main`; rerunning the old run would test its old commit again.

## Original Linux checks (2026-09-14)

Checked against Aether-GUI base commit
`93314fcd97bf6b446d537aac9538b01bef04c7a0`.

| Check | Result |
| --- | --- |
| Frontend TypeScript and Vite production build | Passed |
| Frontend ESLint | Passed |
| Rust desktop backend compile check on Linux | Passed |
| Final-proxy library tests | 11 passed |
| Desktop backend unit tests | 22 passed |
| Final-proxy library Clippy with warnings denied | Passed |
| Git diff whitespace check | Passed |

The relay tests run against local mock servers, including a fake Aether SOCKS5
listener and a separate final proxy. They verify that both hops are used in the
right order, domain names are passed through the proxies, authentication failure
does not cause an alternate connection, early response bytes survive both proxy
handshakes, half-closed TCP streams finish, UDP and BIND are rejected, and stopping
the relay closes active connections and cancels a pending connection check.

The backend tests also cover the new profile defaults, the WireGuard/internal-port
configuration, and removing credentials from saved profiles while retaining the
requirement to authenticate after restarting.

The desktop tests were linked and run on Linux. The build used an isolated set of
Linux system-library files and one code-generation unit for the GTK dependency to
work around an object-file generation issue in this execution environment. The
project's normal Windows build settings were not changed. The compile check also
reports an existing Linux-only unused import in `src-tauri/src/focus.rs`.

A browser-rendered UI check could not run because the test browser download
timed out. The frontend was compiled and linted, but has not been exercised in a
native Windows WebView here. No Windows installer was produced or executed
during those initial local checks; see the later CI results above.

No connection to a real WARP gateway or private final proxy was made. No personal
proxy credentials were requested, stored, or included in this package.

See `FINAL-PROXY.md` for usage, supported behavior and Windows build instructions.
