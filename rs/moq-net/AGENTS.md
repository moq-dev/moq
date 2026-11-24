The wire layer: generic over transport and media-agnostic. Protocol state-machine details (stream instances, live edge, group expiry, route switching) live in `src/lib.rs` and `doc/concept/moq-lite.md`.

# Rules

- Nothing is local-only: every field and behavior survives the wire identically through a relay or in-process. Local-only knobs belong above `moq-net` (`moq-mux`, `hang`).
- Local and remote paths behave identically (local close/cancel/error reaches readers the same as remote, never as a cleaner end). Closes never cascade across groups/tracks/broadcasts.
- Never abandon a stream or producer without a terminal path (expiry, cancel, or session close), or stale streams hold connection credit and stall siblings.
- A relay reports what it knows now and never blocks on peers (only exception: a track listing's synced mark waits for upstream while tracks still flow live).
- New capabilities are additive `SETUP` extensions; receivers ignore unknown parameters. Respond to every feature a peer may use, but don't request new ones.
- IETF-only features (e.g. `FORWARD=0`) stay minimal inside the IETF layer with no public API. Outside `moq-net`, branch on `is_lite()` / `alpn()`; inside, list old drafts explicitly with newest as fallthrough.
- `Timestamp` carries a scale: `.convert()` before arithmetic, seed max with `Option` (never `ZERO`); rewound peer timestamps must never panic or cost unbounded work.
- `connect`/`accept` return `(Session, Driver)`; `Driver` holds no `Session` clone so spawning it never keeps the session alive.

# Testing

- Never use tokio or wall clock: plain `#[test]` with explicit instants, or `#[kio::test]` for async.
- `just rs fuzz <target>` needs nightly; commit crash inputs to `fuzz/regressions/<target>/` (replayed in `just check`).
- Mark Miri-incompatible tests `#[cfg_attr(miri, ignore = "<why>")]`; shrink heavy workloads under `cfg(miri)`.
