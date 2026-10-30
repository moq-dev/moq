The wire layer. Generic over the transport and media-agnostic: the relay never inspects payloads.

# Rules

- New capabilities are additive SETUP extensions; receivers ignore unknown parameters.
- Closes never cascade. Aborting a group must not tear down its track, broadcast, or siblings.
- A route change is never fatal on its own. A subscription stays on the instance it started on while that instance still has a route, even after a newer route wins the path. Once none is left (e.g. the publisher's session closed), it ends with an error, never a clean end, and the app requests the path again. Never stitch it onto another instance. A clean end means only a deliberate publisher finish. Players start, stop, and restart on announcements, never on request errors. See "Publisher epochs" in `doc/concept/moq-lite.md`.
- Local and remote paths behave the same: a local close, cancel, or error reaches local readers exactly as the remote equivalent would, never as a cleaner end.
- The cache is a performance optimization. A relay behaves the same warm or cold: report upstream state (e.g. the largest group), not what the cache happens to hold.
- A relay never blocks an answer on other nodes finishing something (replay, catch-up, a full announce set). Report what is known now; a rare undercount is acceptable.
- A moq-transport feature with no moq-lite counterpart (e.g. FORWARD=0) gets the least code inside the IETF layer and nothing in the model.
- Sessions are caller-driven: `connect`/`accept` return `(Session, Driver)`. The `Driver` holds no `Session` clone, so spawning it never keeps the session alive.
- Version matching: outside the crate branch on `is_lite()` and `alpn()`; inside it match the draft enums. List old drafts explicitly and let the newest behavior be the fallthrough so future drafts fall forward.
- Interop rule of thumb: respond to every feature a peer may use, but don't request new ones.
- `Timestamp` is an instant with a scale, not a scalar. `checked_add`/`checked_sub` error on mismatched scales; `.convert()` first. `ZERO` is second-scale, so seed a max with `Option`, not `ZERO`. A track's timestamps never rewind. A local write refuses one, but some received tracks skip that check: a peer's rewind must never panic or cost unbounded work, yet results on that track may be wrong, so spend nothing on staying correct after one.

# Testing

- Tests never use tokio or the wall clock. Prefer a plain `#[test]` that polls with explicit instants; async bodies use `#[moq_clock::test]`, whose simulated time jumps only when every task is stalled. tokio is for the benches.
- `just rs fuzz <target>` (one per file in `fuzz/fuzz_targets/`) needs nightly. The target bodies live in `src/fuzz.rs`; `fuzz/regressions/<target>/` replays under `just check`, so commit every crash input there.
- `just rs loom` model-checks the concurrent handoffs. A hang is a lost wakeup, not a flake.
