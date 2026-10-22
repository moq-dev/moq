#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${MOQ_LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

# Build moq-net alone: a workspace build unifies its default features back in.
# Normal dependencies exclude the simulation executor and bench-only Tokio.
minimal=$(cargo tree --locked -p moq-net --no-default-features -e normal --prefix none)
if grep -qE '^(tokio|web-async) ' <<<"$minimal"; then
    echo "moq-net without async must not depend on a native runtime" >&2
    exit 1
fi

cargo clippy --locked -p moq-net --no-default-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p moq-net --no-default-features --no-deps
bash sh/rs/nextest.sh cargo nextest run --locked -p moq-net --no-default-features --tests

# A browser leaf chooses its entropy backend. Build a consumer instead of moq-net's
# native-only bench dev-dependencies, with the same dependency versions as the repo.
consumer=$(mktemp -d)
trap 'rm -rf "$consumer"' EXIT
mkdir "$consumer/src"
cat >"$consumer/Cargo.toml" <<EOF
[workspace]
[package]
name = "moq-net-model-check"
version = "0.0.0"
edition = "2024"
[dependencies]
moq-net = { path = "$PWD/rs/moq-net", default-features = false }
getrandom = { version = "0.4", features = ["wasm_js"] }
EOF
cat >"$consumer/src/lib.rs" <<'EOF'
pub fn poll(driver: &mut moq_net::origin::Driver, now: moq_net::time::Instant) {
    let _ = driver.poll(now, &moq_net::kio::Waiter::noop());
}
EOF
cp Cargo.lock "$consumer/Cargo.lock"
# The copied lock only needs the temporary leaf's own package entry added.
cargo check --manifest-path "$consumer/Cargo.toml" --target wasm32-unknown-unknown --target-dir "${CARGO_TARGET_DIR:-target}"
