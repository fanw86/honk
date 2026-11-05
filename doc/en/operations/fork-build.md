# Build the honk and doona forks

In `fanw86/honk`, open **Actions → Fork build → Run workflow**. Select the honk branch, a Linux musl target (`x86_64` or `aarch64`), and the full 40-character commit SHA from `fanw86/doona` to embed. The default SHA is the initial checkout; update it when building UI changes.

The workflow runs standard doona checks and builds both UI editions, packages their notices, and embeds them into a full honk binary with eBPF, Clash API, native API, rprx and jemalloc. It uses the toolchains and cross-build steps from `release.yml`; keep those steps aligned when updating upstream.

Download the binary archive and corresponding source archives from the run's **Artifacts** section within 14 days. Revision files identify both inputs. No release or tag is published. This is build scaffolding: it does not add ShadowQUIC support or replace kernel/runtime interoperability testing. The x86_64 binary receives a version smoke check; the cross-built ARM64 binary needs validation on its target device.

No additional repository secret is needed for the public UI fork. To maintain the local checkout, push to `origin` (your fork) and fetch upstream changes from `upstream`. Use the normal installation guide after downloading a build.

This workflow does not run honk's Rust test suite. A successful artifact build is not the complete upstream CI gate.
