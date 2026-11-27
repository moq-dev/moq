#!/usr/bin/env bash
# Run kio's library tests under ThreadSanitizer (Linux). Observes the schedules that
# happen to run, not all of them: a pass is evidence, not proof. Nightly only, since
# the sanitizer and `-Zbuild-std` are unstable.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

COMPONENTS=rust-src source sh/rs/nightly.sh
host=$(rustc -vV | sed -n 's/^host: //p')
# An explicit --target keeps the sanitizer off build scripts and proc macros, and
# -Zbuild-std instruments std, so its locks and atomics are modeled rather than
# invisible. Its own target dir: no artifact is shared with the stable builds.
RUSTFLAGS="-Zsanitizer=thread" cargo test --locked -p kio --lib --all-features \
    -Zbuild-std --target "$host" --target-dir "${CARGO_TARGET_DIR:-$PWD/target}/tsan"
