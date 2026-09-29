# [M] Bindings close sessions gracefully

## Goal

moq-ffi and every wrapper (py, swift, kt, go, dart, and the generated C/C++) can end a session by draining it the way `moq_net::Session::close` and `moq_tokio::Connection::close` do. Finished tracks deliver their last groups and FIN before the session ends, bounded by the same deadline. OBS and the language bindings stop losing the tail of a publish when they stop.

## Plan

`Session::close` and `Connection::close` landed with drain-before-close (#4430). Today the FFI session (`rs/moq-ffi/src/session.rs`) only has `cancel(code)` and `shutdown()`, and `shutdown` is just `cancel(0)`, which ends it immediately. The draining close can't be named `close`: UniFFI's Kotlin generator already emits `AutoCloseable.close()` to release the handle, which is why `shutdown` exists.

Open question: make `shutdown` itself the async draining close (a breaking change, so `dev` for published wrappers), or keep it and add a separate async draining method (additive, `main`). Lean toward changing `shutdown`, since its doc already promises a graceful shutdown and AGENTS.md prefers breaking `foo` over adding a variant. Keep `cancel` as the immediate path, mirror the result under one name in every wrapper, and follow the Cross-Package Sync table in AGENTS.md, including `doc/lib/*`. IETF sessions still close at once until `quest/m2/ietf-drain-before-close.md` lands.

Public API: moq-ffi and every wrapper; breaking or additive per the question above. Wire: none.
