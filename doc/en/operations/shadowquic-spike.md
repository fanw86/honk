# Validate the candidate ShadowQUIC client

Run **Actions → ShadowQUIC JLS spike → Run workflow** in the honk fork before implementing the outbound. The workflow runs the standalone [spike](../../../spike/README.md) against a reference server pinned to an exact commit. It leaves the production engine unchanged.

A successful run must authenticate with correct credentials, echo TCP payloads using IPv4 and domain addresses over multiple streams, and reject a wrong password. Check the job summary and download `shadowquic-phase0-evidence` within 14 days for toolchain versions, commit IDs, resolved dependency lockfiles, and server/client logs. A failed or timed-out run does not authorize a GO verdict; diagnose its logs first.

Record the actual run URL and results in the integration workspace's `plan.md`. This gate establishes only candidate JLS/TCP compatibility. It does not exercise UDP, 0-RTT, kernel hooks, or the production honk runtime. Local prerequisites and commands are in the spike README.
