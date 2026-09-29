# [M] Bindings close sessions gracefully

## Goal

On dev, `shutdown` in moq-ffi and every wrapper (py, swift, kt, go, dart, and the generated C/C++) drains the session the way `moq_net::Session::close` and `moq_tokio::Connection::close` do. Finished tracks deliver their last groups and FIN before the session ends, bounded by the same deadline. The language bindings stop losing the tail of a publish when they stop. OBS gets this only once it moves off hand-written libmoq onto the generated C++, which [C++ through moq-ffi](/quest/m1/cpp/README.md) owns; libmoq takes no more shutdown work.

## Plan

`Session::close` and `Connection::close` landed with drain-before-close (#4430). Today the FFI session (`rs/moq-ffi/src/session.rs`) only has `cancel(code)` and `shutdown()`, and `shutdown` is just `cancel(0)`, which ends it immediately.

Decision (maintainer, 2026-09-28): `shutdown` becomes the async draining close in every binding. The name can't be `close`, because UniFFI's Kotlin generator already emits `AutoCloseable.close()` to release the handle, and `shutdown` already promises a graceful shutdown. Turning it async is breaking, so this targets `dev`; no second method, per the no-shim rule in AGENTS.md. `cancel` stays the immediate path. Like `Session::close`, `shutdown` is fallible: a drain the peer does not acknowledge in time surfaces as a timeout error through each binding's error mechanism rather than a silent success, with a test for that path.

Mirror `shutdown` in each wrapper and follow the Cross-Package Sync table in AGENTS.md, including `doc/lib/*`. IETF sessions still close at once until [IETF drain before close](/quest/m2/ietf-drain-before-close.md) lands.

Public API: breaking on dev, `shutdown` becomes async, drains, and returns the close error in moq-ffi and every wrapper. Wire: none.
