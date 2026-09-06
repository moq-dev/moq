The wire layer. Generic over the transport and media-agnostic: the relay never inspects payloads.

# Rules

- Any wire change updates the matching draft in `drafts/` and `js/net` in the same PR: `draft-lcurley-moq-lite.md` for session/SETUP/framing, the per-feature draft otherwise. New capabilities are additive SETUP extensions; receivers ignore unknown parameters.
- Closes never cascade. Aborting a group must not tear down its track, broadcast, or siblings.
- Sessions are caller-driven: `connect`/`accept` return `(Session, Driver)`. The `Driver` holds no `Session` clone, so spawning it never keeps the session alive.
- Version matching: outside the crate branch on `is_lite()` / `is_ietf()` and `alpn()`; inside it match the draft enums. List old drafts explicitly and let the newest behavior be the fallthrough so future drafts fall forward.
- Interop rule of thumb: respond to every feature a peer may use, but don't request new ones. Never emit or accept partial groups; a group starts at frame 0.
- `Timestamp` is an instant with a scale, not a scalar. `checked_add`/`checked_sub` error on mismatched scales; `.convert()` first. `ZERO` is second-scale, so seed a max with `Option`, not `ZERO`.

# Testing

- `just rs fuzz <target>` (`lite`, `ietf`, `varint`, `path`) needs nightly. The target bodies live in `src/fuzz.rs`; `fuzz/regressions/<target>/` replays under `just test`, so commit every crash input there.
- `just rs loom` model-checks the concurrent handoffs. A hang is a lost wakeup, not a flake.
- `just test smoke-full` runs the cross-language interop matrix after a wire change.
