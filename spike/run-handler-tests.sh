#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
log="$root/spike/tmp/handler-logs"
mkdir -p "$log"
server_pid=""
stub_pid=""
cleanup() {
    [[ -z "$server_pid" ]] || kill "$server_pid" 2>/dev/null || true
    [[ -z "$stub_pid" ]] || kill "$stub_pid" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cp "$root/spike/reference-Cargo.lock" "$root/shadowquic/Cargo.lock"
cargo build --locked --manifest-path "$root/shadowquic/Cargo.toml" -p shadowquic \
    --no-default-features --features shadowquic-quinn,sunnyquic-noq,ring 2>&1 | tee "$log/server-build.log"
python3 "$root/spike/tools/jls_upstream_stub.py" --listen 127.0.0.1:14431 > "$log/stub.log" 2>&1 &
stub_pid=$!
"$root/shadowquic/target/debug/shadowquic" --config "$root/spike/config/shadowquic-server.yaml" run > "$log/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 60); do
    if grep -q running "$log/server.log"; then break; fi
    if ! kill -0 "$server_pid" 2>/dev/null; then cat "$log/server.log"; exit 1; fi
    sleep 0.25
done
grep -q running "$log/server.log" || { cat "$log/server.log"; exit 1; }
export HONK_SHADOWQUIC_TEST_SERVER=127.0.0.1:14430
cd "$root"
cargo test -p honk-outbound --features shadowquic --test shadowquic --locked -- --ignored --list > "$log/interop-list.txt"
test "$(grep -c ': test$' "$log/interop-list.txt")" -eq 8
cargo test -p honk-outbound --features shadowquic --lib shadowquic_preparation_cancelled_during_promotion_is_joined --locked -- --ignored --list > "$log/promotion-list.txt"
grep -q 'shadowquic_preparation_cancelled_during_promotion_is_joined: test' "$log/promotion-list.txt"
cargo test -p honk-core --features native-api,shadowquic --lib shadowquic_relay --locked -- --ignored --list > "$log/relay-list.txt"
grep -q 'shadowquic_relay_bounds_remote_fin_with_silent_client: test' "$log/relay-list.txt"
env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
    cargo test -p honk-outbound --features shadowquic --test shadowquic --locked -- --ignored --test-threads=1 \
    2>&1 | tee "$log/interop.log"
env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
    cargo test -p honk-outbound --features shadowquic --lib shadowquic_preparation_cancelled_during_promotion_is_joined --locked -- --ignored \
    2>&1 | tee "$log/promotion-cancellation.log"
env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
    cargo test -p honk-core --features native-api,shadowquic --lib shadowquic_relay --locked -- --ignored \
    2>&1 | tee "$log/relay-half-close.log"
