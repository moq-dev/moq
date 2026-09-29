# [M] Bindings close sessions gracefully

## Goal

moq-ffi and every wrapper (py, swift, kt, go, dart, and the generated C/C++) expose a close that drains the session the way `moq_net::Session::close` and `moq_tokio::Connection::close` do. Finished tracks deliver their last groups and FIN before the session ends, bounded by the same deadline. OBS and the language bindings stop losing the tail of a publish when they stop.

## Plan

`Session::close` and `Connection::close` landed with drain-before-close (#4430). Today the bindings can only abort or drop a session (`rs/moq-ffi/src/session.rs`), which ends it immediately. Add an async draining close to the FFI session and connection handles, mirror it in each wrapper under the same name, and keep abort as the immediate path. Follow the Cross-Package Sync table in AGENTS.md, including `doc/lib/*`. IETF sessions still close at once until `quest/m2/ietf-drain-before-close.md` lands.

Public API: additive in moq-ffi and every wrapper. Wire: none.
