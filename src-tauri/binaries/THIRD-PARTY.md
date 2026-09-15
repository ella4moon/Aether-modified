# Bundled whole-laptop component (Windows)

Windows installers include the unmodified **sing-box 1.14.1** distribution from
[SagerNet/sing-box](https://github.com/SagerNet/sing-box/releases/tag/v1.14.1):
`sing-box.exe`, its companion `libcronet.dll`, and `sing-box-LICENSE.txt`.
Wintun support is embedded in this upstream executable; no separate driver
download or global sing-box installation is performed by Aether-GUI.

sing-box is licensed under **GPL-3.0-or-later**, with upstream's additional
naming/affiliation terms reproduced in `sing-box-LICENSE.txt`. We retain its
original executable name. This modified Aether-GUI project is independent of
SagerNet and does not imply its endorsement.

- [Exact corresponding source](https://github.com/SagerNet/sing-box/tree/v1.14.1)
- [Source archive](https://github.com/SagerNet/sing-box/archive/refs/tags/v1.14.1.tar.gz)
- [Upstream build instructions](https://sing-box.sagernet.org/installation/build-from-source/)
- [Dependency versions and source modules](https://github.com/SagerNet/sing-box/blob/v1.14.1/go.mod)
- [Embedded Wintun component and license](https://github.com/SagerNet/sing-tun/tree/v0.9.3/internal/wintun)

The download script fixes both the version and Windows archive SHA-256:
`5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89`.
