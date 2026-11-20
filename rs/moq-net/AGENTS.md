The wire layer: generic over the transport and media-agnostic.

# Rules

- Nothing is local-only: every field and behavior survives the wire, so it means the same through a relay as in-process. Local-only knobs belong above moq-net (moq-mux, hang).
- Local and remote paths behave the same: a local close, cancel, or error reaches readers exactly as the remote one would, never as a cleaner end.
- Closes never cascade: aborting a group leaves its track, broadcast, and siblings alone.
- Never finish a stream or producer and forget it: something, such as expiry, a cancel, or the session's close, must abort it eventually, or a stale stream holds connection credit and stalls the streams behind it.
- A subscription stays on the instance it started on while that instance has a route, even after a newer route wins the path. When none is left, it ends with an error, never a clean end, and never stitches onto another instance. A clean end means only a deliberate publisher finish. Apps restart on announcements, never on request errors. See "Stream instances" in `doc/concept/moq-lite.md`.
- The live position (largest group, live edge) comes only from an open upstream subscription's answer: a copy with none open waits for the next `SUBSCRIBE_OK`, since a cached group 10 says only that group 10 existed. A partial hit, such as a group missing its first frames, still asks upstream for the rest.
- A relay never blocks an answer on other nodes finishing something; it reports what it knows now. One exception: a track listing's synced mark waits for the upstream's, while its tracks still flow live.
- Groups never overlap in time: every frame of a lower group presents before every frame of a higher one, though frames within a group may reorder (B-frames). Expire a group by timestamp, from its End once whole (`GROUP_END`, at or above its highest frame) and otherwise from the lowest timestamp known in any higher group, and by wall clock, once its successor arrived longer ago than the budget, which stops a publisher whose timestamps stall. The latest group never expires.
- A route switch for the same instance looks like one subscription split at one boundary, picked at the switch: the largest group any copy holds, past the frames held contiguously in it. The new subscription starts there; the old one is capped at that group and drains what lies below within the budget. Deliver each frame once, from whichever copy brings it first.
- New capabilities are additive SETUP extensions; receivers ignore unknown parameters.
- A moq-transport feature with no moq-lite counterpart (e.g. FORWARD=0) gets the least code inside the IETF layer. It may keep crate-private state in the model so a moq-transport-to-moq-transport relay forwards it faithfully, but never adds public API.
- Outside the crate, branch on `is_lite()` and `alpn()`. Inside, match draft enums with old drafts listed explicitly and the newest behavior as the fallthrough.
- Respond to every feature a peer may use, but don't request new ones.
- `Timestamp` is an instant with a scale: `.convert()` before arithmetic, and seed a max with `Option`, not `ZERO`. A peer's rewound timestamp must never panic or cost unbounded work, but needn't give correct results.
- `connect`/`accept` return `(Session, Driver)`; the `Driver` holds no `Session` clone, so spawning it never keeps the session alive.

# Testing

- Tests never use tokio or the wall clock: plain `#[test]` with explicit instants, or `#[kio::test]` for async bodies.
- `just rs fuzz <target>` needs nightly. Commit every crash input to `fuzz/regressions/<target>/`, which replays under `just check`.
- A test Miri can't run carries `#[cfg_attr(miri, ignore = "<why>")]`; shrink heavy workloads under `cfg(miri)` instead.
