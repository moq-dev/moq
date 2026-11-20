The wire layer: generic over the transport and media-agnostic.

# Rules

- Nothing is local-only: every field and behavior survives the wire, so it means the same through a relay as in-process. Local-only knobs belong above moq-net (moq-mux, hang).
- Local and remote paths behave the same: a local close, cancel, or error reaches readers exactly as the remote one would, never as a cleaner end.
- Closes never cascade: aborting a group leaves its track, broadcast, and siblings alone.
- Never finish a stream or producer and forget it: something, such as expiry, a cancel, or the session's close, must abort it eventually, or a stale stream holds connection credit and stalls the streams behind it.
- A subscription stays on the instance it started on while that instance has a route, even after a newer route wins the path. When none is left, it ends with an error, never a clean end, and never stitches onto another instance. A clean end means only a deliberate publisher finish. Apps restart on announcements, never on request errors. See "Stream instances" in `doc/concept/moq-lite.md`.
- Serve the part of a subscription the cache can prove and ask upstream for every miss, including missing frames. When the live subscription has ended, wait for the new `SUBSCRIBE_OK` before reporting a live position. Group 10 in an idle cache says only that group 10 existed.
- Forward known results as they arrive. A track listing streams tracks immediately and reports synced once upstream does.
- Frame timestamps may reorder within a group (B-frames). Every timestamp in a lower group must be at or before every timestamp in a higher group. For timestamp-based skipping, use a whole group's maximum timestamp. For an incomplete group, use the minimum known timestamp of a higher group as an upper bound. Without such a bound, its frames may still be in range.
- When switching routes for the same instance, cap the old subscription and start the new one at the same position, just past the frames held contiguously in the largest known group. Drain the old range while possible, accounting for every requested group or asking the new route for what is missing. Use in-flight frames from either copy when available and deliver each frame once on the same logical subscription.
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
