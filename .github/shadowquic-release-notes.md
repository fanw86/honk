Experimental ShadowQUIC fork prerelease for Linux x86_64 and ARM64.

## Highlights

- JLS-authenticated ShadowQUIC TCP and UDP, including datagram and stream modes.
- Embedded full and lite doona UIs with ShadowQUIC forms and validation.
- Static musl binaries with eBPF, native/Clash APIs and jemalloc.

## Sources and verification

The release tag identifies the exact honk source. Embedded doona is pinned to `4c45065cdf47caaf3788a4c034c1ff29b18a25b6`, which passed its complete UI checks, builds and targeted Chromium flows. Both corresponding-source archives and UI notices are included. `BUILD-INFO.txt` inside each binary archive records revisions, target, features and workflow run.

Download the binary archive for your architecture and verify it with `sha256sum -c SHA256SUMS --ignore-missing`. To verify every published file, download all four archives and run `sha256sum -c SHA256SUMS`.

## Installation

Extract the selected binary archive and read `SHADOWQUIC.md` or `SHADOWQUIC.zh.md`. Install `honk-core` at `/usr/bin/honk-core` to match the included service. Preserve existing configuration and configure interfaces, routing, DNS and real JLS server credentials before starting. Select `ui: embedded` or `ui: embedded-lite` for the UI edition; keep the native API on loopback and use SSH forwarding for remote access.

## Validation limits

Both tagged cross-builds, source identities, checksum inventories, manifests and package contents are checked before publication. The packaged x86_64 binary runs version/help checks. ARM64 is cross-compiled only. Prior integration tests cover the real reference server's TCP/UDP/authentication/lifecycle behavior. This prerelease does not establish deployment or gateway/kernel/NFQUEUE acceptance on your hardware. Early application-data replay remains disabled. No container images or standalone doona release are published by this workflow.
