# Troubleshooting final-proxy UDP

If Aether reports **“Final proxy UDP association failed … SOCKS code 7”**, the
final SOCKS5 endpoint rejected the `UDP ASSOCIATE` command. In this connection
check, authentication and the TCP CONNECT check have already succeeded. This
error occurs before the relay starts forwarding UDP packets; it is separate
from the WireGuard tunnel handshake and Windows whole-laptop routing.

[RFC 1928](https://www.rfc-editor.org/rfc/rfc1928.html), section 6, defines reply
`0x07` as “Command not supported.” The relay requests an unknown UDP source as
`0.0.0.0:0`, as required by the RFC when its eventual source address and port are
unknown. The request bytes are:

```text
05 03 00 01 00 00 00 00 00 00
```

A provider advertising UDP support may still need to check the particular
endpoint, port, account, or source policy. For example,
[Proxy-Seller describes SOCKS5 UDP ASSOCIATE support](https://proxy-seller.com/blog/socks5-proxy-benefits-use-cases/).
A refusal alone does not establish why that endpoint rejected the command.

## Compare with a direct connection

The standard-library Node.js diagnostic in
[`scripts/check-socks5-udp.mjs`](scripts/check-socks5-udp.mjs) makes two separate
TCP control connections: one requests TCP CONNECT to `example.com:443`, and one
requests UDP ASSOCIATE. It does not change any routes or proxy settings.

1. Disconnect Aether whole-laptop mode and any other VPN/TUN apps so the test
   uses your normal connection.
2. With Node.js 18 or newer installed, run from the repository directory:

   ```powershell
   node .\scripts\check-socks5-udp.mjs PROXY_HOST PROXY_PORT
   ```

   If you downloaded just the script, use `node .\check-socks5-udp.mjs` followed
   by your proxy host and port instead.
3. Enter your proxy username and password at the prompts. The password prompt
   is visible while typing. The script keeps credentials in memory and excludes
   them from the JSON results.
4. Copy the two JSON result blocks for diagnosis. Include whether Aether and
   other VPN/TUN apps were disconnected during the test.

| Direct result | What it establishes |
| --- | --- |
| TCP accepted; UDP reply `7` | The endpoint also rejects the standard UDP command outside Aether. Ask the provider whether native SOCKS5 UDP ASSOCIATE is enabled for this account and port, and include the request/reply bytes. |
| TCP accepted; UDP reply `0`, with no error | The endpoint accepts a direct UDP association. If the same command fails through Aether, investigate the source/path difference and provider policy or compatibility. |
| Authentication error, timeout, or connection error | UDP support is not established. Resolve the reported stage before interpreting UDP behavior. |
| Any `error` field | The operation did not complete successfully, even if part of a success reply was received. |

An accepted association is **not** proof of UDP packet delivery or the final
exit IP. This script sends no UDP datagrams and closes the association after
reading its reply. End-to-end UDP forwarding must be checked separately once
the association succeeds.

The application still requires both proxy hops to accept UDP when Forward UDP
is enabled. It does not silently disable UDP or send it directly when a hop
refuses the command.
