The wire layer: generic over the transport and media-agnostic.

# Rules

- Nothing is local-only: every field and behavior survives the wire, so it means the same through a relay as in-process. Local-only knobs belong above moq-net (moq-mux, hang).
- Local and remote paths behave the same: a local close, cancel, or error reaches readers exactly as the remote one would, never as a cleaner end.
- Closes never cascade: aborting a group leaves its track, broadcast, and siblings alone.
- A subscription stays on the instance it started on while that instance has a route, even after a newer route wins the path. When none is left, it ends with an error, never a clean end, and never stitches onto another instance. A clean end means only a deliberate publisher finish. Apps restart on announcements, never on request errors. See "Stream instances" in `doc/concept/moq-lite.md`.
- The cache is only an optimization: a relay behaves the same warm or cold, reporting upstream state (such as the largest group) rather than what it holds.
- A relay never blocks an answer on other nodes finishing something; it reports what it knows now.
- New capabilities are additive SETUP extensions; receivers ignore unknown parameters.
- A moq-transport feature with no moq-lite counterpart (e.g. FORWARD=0) gets the least code inside the IETF layer and nothing in the model.
- Outside the crate, branch on `is_lite()` and `alpn()`. Inside, match draft enums with old drafts listed explicitly and the newest behavior as the fallthrough.
- Respond to every feature a peer may use, but don't request new ones.
- `Timestamp` is an instant with a scale: `.convert()` before arithmetic, and seed a max with `Option`, not `ZERO`. A peer's rewound timestamp must never panic or cost unbounded work, but needn't give correct results.
- `connect`/`accept` return `(Session, Driver)`; the `Driver` holds no `Session` clone, so spawning it never keeps the session alive.

# Testing

- Tests never use tokio or the wall clock: plain `#[test]` with explicit instants, or `#[kio::test]` for async bodies.
- `just rs fuzz <target>` needs nightly. Commit every crash input to `fuzz/regressions/<target>/`, which replays under `just check`.
- A test Miri can't run carries `#[cfg_attr(miri, ignore = "<why>")]`; shrink heavy workloads under `cfg(miri)` instead.
