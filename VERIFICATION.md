# Verification of the final-proxy changes

Checked on 2026-09-14 against Aether-GUI base commit
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
native Windows WebView here. No Windows installer was produced or executed.

No connection to a real WARP gateway or private final proxy was made. No personal
proxy credentials were requested, stored, or included in this package.

See `FINAL-PROXY.md` for usage, supported behavior and Windows build instructions.
