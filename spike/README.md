# ShadowQUIC Phase 0 JLS spike

This standalone crate tests a candidate JLS client against the real reference
server. It does not modify or depend on production honk code.

## Run in GitHub Actions

Open **Actions > ShadowQUIC JLS spike > Run workflow** in the honk fork.
The workflow pins the server revision, uses honk's host Rust pin, runs both
credential scenarios, and uploads logs, revisions and resolved Cargo lockfiles
as `shadowquic-phase0-evidence` for 14 days, including on failure.

## Run locally

Requires Rust 1.88+, Python 3, Bash, and a C compiler for ring. From the honk root,
check out `spongebob888/shadowquic` at
`ef1c7eb788b087b7e0b3ac99ce9b72f8068f53b1` into `shadowquic/`, then run:

```bash
bash spike/run-spike.sh --rebuild-server
```

The reference checkout must be beside `spike/`. The harness builds the server
with `shadowquic-quinn,sunnyquic-noq,ring` and no default features. It starts a
loopback TCP stub upstream on port 14431 and the server on UDP port 14430.
`SP_PORTS="15430 15431"` overrides those ports. Logs/configuration go into
`spike/tmp/`; the harness terminates its child processes on exit.

## Acceptance and limitations

The positive case must report `is_jls=Some(true)` and return identical payloads
through IPv4 and domain SOCKS5 addresses and a second stream. The negative case
must not authenticate with a wrong password. A stalled negative handshake is
accepted only alongside a successful positive case. Any scenario failure fails
the harness. Client `jls_chosen_user()` is recorded, without assuming its value.

Both cases must print `SPIKE RESULT: PASS` before recording a TCP/JLS GO. This
does not establish UDP, 0-RTT, production routing, or honk runtime integration.
Generated lockfiles are evidence, not yet committed dependency pins for this
experimental crate. Record the run URL and observed results in the integration
workspace's `plan.md` before implementing the production outbound.

## Production-handler interoperability

The optional `shadowquic` outbound is tested by `bash spike/run-handler-tests.sh`.
It uses the same pinned server checkout, copies `reference-Cargo.lock` from
Phase 0 run 42139824856 (SHA-256
`0604c1ab71e76174056b850c94c5684f2e11d148b4f4f85a36e3e46de4a2602e`),
and runs the actual handler's ignored TCP/UDP, lifecycle and core relay tests.
Logs go to `spike/tmp/handler-logs/`. The manual configuration integration
workflow accepts `shadowquic=true` to run these tests plus feature boundaries,
lint and compilation. It preserves the resolved honk lockfile and formatting
patch as artifacts. These tests do not exercise real eBPF gateway routing.
