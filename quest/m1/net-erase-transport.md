# [L] moq-net compiles its protocol code once

## Goal

`moq-net`'s session drivers compile once per binary, not once per transport
session type and per crate that drives them. The relay, `libmoq_ffi.so`,
`libmoq`, the CLI and `@moq/wasm` shrink by the duplicated copies, and the
relay fan-out benchmark shows no regression beyond its noise.

## Plan

Found by the 2026-10 size audit (main at
2b28f5eac). The drivers are generic over the session, so each backend
(QUIC/WebTransport, qmux) gets a full copy, the IETF `ControlStreamAdapter`
doubles the IETF half, and every crate that awaits a `moq-tokio` future or
calls `moq_net::Server`/`Client` with a concrete session codegens its own copy:
release builds share no generics across crates. `moq-net` is 9.1 of 23.5 MiB of
Rust text in the relay (eight copies), 6.4 of 19.2 MiB in the FFI library and
7.4 of 36.5 MiB in the CLI. Estimated saving: 5 to 6 MiB of the relay, 4 to
5 MiB of the FFI library, similar for `libmoq` and the CLI.

Open decision (public API): `Driver<S>`, `server::Handshake<S>` and friends
carry the session type.

1. Drop the type parameter: the public types take a boxed session trait object
   at their constructor. Breaking, and the API gets simpler.
2. Keep the generic signatures as thin entry points that erase at once.
   Non-breaking, but it is a compatibility shim the repo rules refuse.
3. Leave it, accepting the size.

Recommendation: 1, if the dynamic dispatch prices out. Measure it first with
`bench/relay.sh` on `bench/workloads/fanout-heavy.toml` and the `moq-net`
session benchmark, before and after. The async bodies must end up
non-generic inside `moq-net`, or each awaiting crate still instantiates them.
Re-measure with the audit's method: `nm` symbol attribution by crate path and
by instantiating crate on an unstripped `profiling` build.
