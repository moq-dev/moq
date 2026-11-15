# [XS] The carla bridge's tests leave no lock files

## Goal

Running the carla bridge's tests (`demo/carla/bridge`) leaves no
`carla-bridge-*.sock.lock` file, or socket, behind in TMPDIR.

## Plan

Found 2026-11-14 by [kixel-ai/dotfiles#164](https://github.com/kixel-ai/dotfiles/pull/164)'s
trace of what moq leaves in TMPDIR. `Bridge::listen` (`src/lib.rs`) takes an
flock on `<socket>.lock` and never removes it. That is right in production:
unlinking a lock file another process may be opening lets two bridges lock
different inodes at one path. The tests in `src/lib.rs`, `src/server.rs`, and
`tests/gpu.rs` name sockets `std::env::temp_dir()/carla-bridge-*-<pid>.sock`,
so every run leaves a new lock file.

Recommendation: each test puts its socket in a temporary directory it owns and
removes, such as with a `tempfile` dev-dependency, leaving `Bridge` unchanged.

Fixes [moq-dev/moq#5621](https://github.com/moq-dev/moq/pull/5621), which added
the lock and the tests.

Public API: none. Wire: none.
