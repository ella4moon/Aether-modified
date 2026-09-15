# Verification of the final-proxy changes

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
