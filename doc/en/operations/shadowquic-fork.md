# ShadowQUIC integration fork

Versioned fork prereleases use `shadowquic-vMAJOR.MINOR.PATCH-beta.N` tags and the dedicated **ShadowQUIC release** workflow. It rebuilds both architectures with the pinned UI and publishes verified binaries, corresponding sources and `SHA256SUMS`. Standard `v*` tags use the separate upstream release configuration.

These experimental packages combine the optional JLS-authenticated ShadowQUIC handler with the tested full and lite doona UIs. Standard upstream releases do not include this fork integration. The manual **Fork build** workflow accepts `shadowquic=true`, a full `doona_sha`, and either `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`. Default compilation remains unchanged.

## Package and source verification

Download the binary artifact and matching `fork-sources-<target>` artifact from the same Actions run. Extract both ZIP downloads into one directory, then run:

```sh
sha256sum -c SHA256SUMS
mkdir -p unpacked
tar -xzf honk-core-*.tar.gz --strip-components=1 -C unpacked
cat unpacked/BUILD-INFO.txt
```

The manifest records both source revisions, target, features and workflow run. Keep the two source archives, licenses and both UI notice directories alongside redistributed binaries. Actions artifacts expire; download them while retained. This workflow creates artifacts, not a GitHub Release.

## Installation

Choose the architecture matching `uname -m` (`x86_64` or `aarch64`). These are Linux binaries; Windows cannot execute them. Follow the [README installation requirements](../../../README.md#install) for kernel, interfaces, routing and DNS configuration.

```sh
sudo install -m 755 unpacked/honk-core /usr/bin/honk-core
sudo install -m 644 unpacked/honk-core.service /etc/systemd/system/honk-core.service
sudo mkdir -p /etc/honk
```

Preserve any existing binary/configuration before replacing it. For a new installation, copy `unpacked/config.min.dae` to `/etc/honk/config.dae` and edit its interfaces, DNS and routing for the host. Its SOCKS node and interface names are examples, not a ready-to-run gateway configuration. Replace its node/group/routing sections as appropriate:

```dae
node {
    sq: 'shadowquic://USERNAME:PASSWORD@SERVER:443?sni=JLS_SERVER_NAME#sq'
}
group {
    shadow { filter: name('sq')
             policy: fixed(0) }
}
routing { fallback: shadow }
```

Percent-encode reserved credential characters. SNI must match the actual JLS server. Use [node options](../reference/nodes.md#experimental-shadowquic-configuration) for UDP stream mode, ALPN, MTU and half-close tuning. Add local bypass/routing rules suitable for the host before enabling transparent proxying.

## Embedded UI and editable configuration

Add `include { 'auth.dae' }` to the main configuration. Create `/etc/honk/auth.dae` locally, replacing the token with a long random value:

```dae
experimental {
    native_api {
        enabled: true
        listen: '127.0.0.1:9527'
        secret: 'REPLACE_WITH_A_LONG_RANDOM_TOKEN'
        config_write: true
        ui: embedded
    }
}
```

Keep this credential include read-only to API writes; the engine enforces that boundary. The main source can then hold editable nodes/groups. Select `ui: embedded-lite` for lite instead; both editions are embedded, and the chosen edition is served at `/ui/`. Changing native settings requires a local edit and restart. See [native API configuration](../reference/experimental.md#native_api) for authentication and write restrictions.

After reviewing the complete configuration:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now honk-core
sudo journalctl -u honk-core -n 100 --no-pager
```

From your workstation, forward the loopback API with `ssh -L 9527:127.0.0.1:9527 USER@HOST`, open `http://127.0.0.1:9527/ui/`, and enter the token. The ShadowQUIC form requires username, password and JLS SNI. Native plain/Base64 URI subscriptions are supported; structured Clash/sing-box/Surge ShadowQUIC dialects are not.

## Validation limits

The handler has reference-server TCP/UDP/authentication/lifecycle tests and both UIs have unit/build/targeted Chromium checks. Packaging verifies embedded eBPF BTF and archives; x86_64 additionally executes packaged `--version` and `--help`. ARM64 is cross-compiled, not executed. These checks do not verify a real gateway's eBPF, kernel/NFQUEUE behavior, routing or deployment. Early application-data replay remains disabled. Confirm connectivity, UDP, API access and clean shutdown on your target before relying on this experimental build.
