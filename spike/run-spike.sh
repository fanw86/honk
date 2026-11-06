#!/usr/bin/env bash
# Phase 0 spike runner: build the reference ShadowQUIC server, start it with a
# stub jls-upstream, then run the honk-side JLS client spike twice:
#
#   1. correct credentials  -> JLS must authenticate, TCP echo must round-trip
#   2. wrong credentials    -> must NOT authenticate
#
# Usage:  spike/run-spike.sh [--rebuild-server]
# Env:    SP_PORTS="14430 14431"  override server / jls-upstream ports
#         SP_USER / SP_PASS / SP_SNI override credentials
#
# Works in Linux/macOS bash and in Git Bash on Windows.

set -euo pipefail

SPIKE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SPIKE_DIR/.." && pwd)"
TMP="$SPIKE_DIR/tmp"
LOG="$TMP/logs"
CFG="$TMP/shadowquic-server.yaml"

SP_USER="${SP_USER:-spike-user}"
SP_PASS="${SP_PASS:-spike-pass}"
SP_SNI="${SP_SNI:-shadowquic.test}"
read -r SP_PORT JLS_PORT <<<"${SP_PORTS:-14430 14431}"

REBUILD=0
[ "${1:-}" = "--rebuild-server" ] && REBUILD=1

say() { printf '\n== %s\n' "$*"; }

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

require cargo
PY="$(command -v python3 || command -v python || true)"
[ -n "$PY" ] || { echo "missing required command: python3 (or python)" >&2; exit 1; }

mkdir -p "$LOG"

# --- 1. build the reference server (minimal features: no plugin/tun/tproxy) ---
SERVER_BIN="$ROOT/shadowquic/target/debug/shadowquic"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) SERVER_BIN="$SERVER_BIN.exe" ;;
esac

if [ "$REBUILD" = 1 ] || [ ! -x "$SERVER_BIN" ]; then
  say "building shadowquic server (debug, minimal features)"
  cargo build \
    --manifest-path "$ROOT/shadowquic/Cargo.toml" \
    -p shadowquic \
    --no-default-features \
    --features shadowquic-quinn,sunnyquic-noq,ring 2>&1 | tee "$LOG/server-build.log"
fi
[ -x "$SERVER_BIN" ] || { echo "server binary not found: $SERVER_BIN" >&2; exit 1; }

# --- 2. render config from the template ---
sed -e "s/127\.0\.0\.1:14430/127.0.0.1:$SP_PORT/" \
    -e "s/127\.0\.0\.1:14431/127.0.0.1:$JLS_PORT/" \
    -e "s/username: \"spike-user\"/username: \"$SP_USER\"/" \
    -e "s/password: \"spike-pass\"/password: \"$SP_PASS\"/" \
    -e "s/server-name: \"shadowquic.test\"/server-name: \"$SP_SNI\"/" \
    "$SPIKE_DIR/config/shadowquic-server.yaml" >"$CFG"

SERVER_PID=""
STUB_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# --- 3. start the stub upstream, then the server ---
say "starting stub jls-upstream on 127.0.0.1:$JLS_PORT"
"$PY" "$SPIKE_DIR/tools/jls_upstream_stub.py" --listen "127.0.0.1:$JLS_PORT" \
  >"$LOG/jls-upstream.log" 2>&1 &
STUB_PID=$!

say "starting shadowquic server on 127.0.0.1:$SP_PORT"
"$SERVER_BIN" --config "$CFG" run >"$LOG/server.log" 2>&1 &
SERVER_PID=$!

# Readiness: the server logs "shadowquic <version> running" after binding.
for _ in $(seq 1 60); do
  if grep -q "running" "$LOG/server.log" 2>/dev/null; then break; fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "server exited during startup; log:" >&2
    cat "$LOG/server.log" >&2
    exit 1
  fi
  sleep 0.25
done
if ! grep -q "running" "$LOG/server.log" 2>/dev/null; then
  echo "server did not report startup within 15s; log:" >&2
  cat "$LOG/server.log" >&2
  exit 1
fi

# --- 4. run the spike: positive then negative ---
COMMON=(--server "127.0.0.1:$SP_PORT" --sni "$SP_SNI" --user "$SP_USER")

say "spike 1/2: correct credentials (expect JLS auth + echo)"
set +e
cargo run --quiet --manifest-path "$SPIKE_DIR/shadowquic-jls-spike/Cargo.toml" -- \
  "${COMMON[@]}" --password "$SP_PASS" --alpn h3 \
  --target-domain localhost \
  --expect ok 2>&1 | tee "$LOG/spike-ok.log"
OK_STATUS=${PIPESTATUS[0]}
set -e

say "spike 2/2: wrong password (expect no JLS auth)"
set +e
cargo run --quiet --manifest-path "$SPIKE_DIR/shadowquic-jls-spike/Cargo.toml" -- \
  "${COMMON[@]}" --password "${SP_PASS}-wrong" --alpn h3 \
  --expect authfail 2>&1 | tee "$LOG/spike-authfail.log"
FAIL_STATUS=${PIPESTATUS[0]}
set -e

say "summary"
grep -h "SPIKE RESULT" "$LOG"/spike-*.log || true
echo "logs: $LOG"

if [ "$OK_STATUS" -eq 0 ] && [ "$FAIL_STATUS" -eq 0 ]; then
  echo "RESULT: PASS — JLS path is viable for a native honk outbound"
  exit 0
fi
echo "RESULT: FAIL — see logs above; native work should pause (plan.md §9/§10)" >&2
exit 1
