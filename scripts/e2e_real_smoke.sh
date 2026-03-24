#!/usr/bin/env bash
set -u

HB="${HB:-./target/debug/hyperbox}"
ADDR="${HB_SMOKE_ADDR:-127.0.0.1:50131}"
URL="http://$ADDR"
ROOT="${TMPDIR:?TMPDIR must be set}/hb-e2e-real-$$"
LOG="$ROOT/server.log"
PASS=0
FAIL=0

mkdir -p "$ROOT"

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$ROOT"
}
trap cleanup EXIT

record_pass() {
  PASS=$((PASS + 1))
  printf "PASS  %s\n" "$1"
}

record_fail() {
  FAIL=$((FAIL + 1))
  printf "FAIL  %s\n" "$1"
}

run_expect_ok() {
  local name="$1"
  local cmd="$2"
  local out
  if out=$(bash -lc "$cmd" 2>&1); then
    record_pass "$name"
    printf "%s\n" "$out" >"$ROOT/${name// /_}.ok.log"
  else
    record_fail "$name"
    printf "%s\n" "$out" >"$ROOT/${name// /_}.fail.log"
  fi
}

run_expect_fail() {
  local name="$1"
  local cmd="$2"
  local out
  if out=$(bash -lc "$cmd" 2>&1); then
    record_fail "$name"
    printf "%s\n" "$out" >"$ROOT/${name// /_}.unexpected_ok.log"
  else
    record_pass "$name"
    printf "%s\n" "$out" >"$ROOT/${name// /_}.expected_fail.log"
  fi
}

json_field() {
  local json="$1"
  local field="$2"
  python -c "import json,sys; print(json.loads(sys.argv[1])[sys.argv[2]])" "$json" "$field"
}

expect_template() {
  local name="$1"
  local workspace="$2"
  local expected="$3"
  local out
  if ! out=$(HYPERBOX_BACKEND=local "$HB" --server-url "$URL" create --template auto --workspace "$workspace" --json 2>&1); then
    record_fail "$name"
    printf "%s\n" "$out" >"$ROOT/${name// /_}.fail.log"
    return
  fi

  local json_line
  json_line=$(printf "%s\n" "$out" | tail -n 1)
  local actual
  actual=$(json_field "$json_line" "template")
  local sid
  sid=$(json_field "$json_line" "sandbox_id")
  HYPERBOX_BACKEND=local "$HB" --server-url "$URL" destroy --sandbox-id "$sid" >/dev/null 2>&1 || true

  if [[ "$actual" == "$expected" ]]; then
    record_pass "$name"
    printf "%s\n" "$out" >"$ROOT/${name// /_}.ok.log"
  else
    record_fail "$name"
    printf "expected=%s actual=%s\n%s\n" "$expected" "$actual" "$out" >"$ROOT/${name// /_}.fail.log"
  fi
}

printf "Starting server at %s\n" "$ADDR"
HYPERBOX_BACKEND=local "$HB" serve --addr "$ADDR" >"$LOG" 2>&1 &
SERVER_PID=$!
sleep 1

if ! kill -0 "$SERVER_PID" 2>/dev/null; then
  printf "Server failed to start. Logs:\n"
  cat "$LOG"
  exit 1
fi

# Core create/destroy
CREATE_JSON=$(HYPERBOX_BACKEND=local "$HB" --server-url "$URL" create --template python:3.12 --json 2>/dev/null)
CREATE_ID=$(json_field "$CREATE_JSON" "sandbox_id")
run_expect_ok "destroy created sandbox" "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" destroy --sandbox-id \"$CREATE_ID\""

# Resume behavior (default run affinity)
run_expect_ok "run writes marker in reusable session" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template python:3.12 --cmd \"echo reuse-ok > .hb_reuse_marker\""
run_expect_ok "run reads marker from same reusable session" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template python:3.12 --cmd \"cat .hb_reuse_marker\""

# Named resume behavior
run_expect_ok "create named sandbox" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" create --name hb-e2e-named --template python:3.12"
run_expect_ok "run against named sandbox" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --name hb-e2e-named --cmd \"echo named-ok > .hb_named_marker\""
run_expect_ok "read from named sandbox" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --name hb-e2e-named --cmd \"cat .hb_named_marker\""
run_expect_ok "destroy named sandbox" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" destroy --name hb-e2e-named"

# Template manifest detection
mkdir -p "$ROOT/rust" "$ROOT/go" "$ROOT/node" "$ROOT/python" "$ROOT/empty"
cat >"$ROOT/rust/Cargo.toml" <<'EOF'
[package]
name = "hb-smoke-rust"
version = "0.1.0"
edition = "2021"
EOF
echo "module example.com/hbsmoke" >"$ROOT/go/go.mod"
echo '{"name":"hb-smoke-node","version":"1.0.0"}' >"$ROOT/node/package.json"
cat >"$ROOT/python/pyproject.toml" <<'EOF'
[project]
name = "hb-smoke-python"
version = "0.1.0"
EOF

expect_template "template auto rust manifest" "$ROOT/rust" "rust:1.75"
expect_template "template auto go manifest" "$ROOT/go" "golang:1.22"
expect_template "template auto node manifest" "$ROOT/node" "node:20"
expect_template "template auto python manifest" "$ROOT/python" "python:3.12"
expect_template "template auto fallback empty workspace" "$ROOT/empty" "python:3.12"

# Command-hint detection
run_expect_ok "template auto command hint rust" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template auto --workspace \"$ROOT/empty\" --cmd \"cargo --version >/dev/null\" --ephemeral --json"
run_expect_ok "template auto command hint go" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template auto --workspace \"$ROOT/empty\" --cmd \"go version >/dev/null\" --ephemeral --json"
run_expect_ok "template auto command hint node" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template auto --workspace \"$ROOT/empty\" --cmd \"node --version >/dev/null\" --ephemeral --json"
run_expect_ok "template auto command hint python" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template auto --workspace \"$ROOT/empty\" --cmd \"python3 --version >/dev/null\" --ephemeral --json"

# Ensure runs once
run_expect_ok "ensure runs first time" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template python:3.12 --ensure \"test ! -f .hb_once && echo once > .hb_once\" --cmd \"cat .hb_once\""
run_expect_ok "ensure skipped second time with marker preserved" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template python:3.12 --ensure \"test ! -f .hb_once && echo once > .hb_once\" --cmd \"cat .hb_once\""

# Failure edges
run_expect_fail "allowlist with no allow domains fails" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template python:3.12 --network allowlist --cmd \"echo never\""
run_expect_fail "allow flag without allowlist mode fails" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --template python:3.12 --allow example.com --cmd \"echo never\""
run_expect_fail "network flag with existing sandbox flags conflict" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" run --name hb-e2e-missing --network none --cmd \"echo never\""
run_expect_fail "unknown template fails" \
  "HYPERBOX_BACKEND=local \"$HB\" --server-url \"$URL\" create --template missing:1 --json"

printf "\nSummary: pass=%d fail=%d\n" "$PASS" "$FAIL"
if [[ "$FAIL" -gt 0 ]]; then
  printf "Failure logs are under %s\n" "$ROOT"
  exit 1
fi
