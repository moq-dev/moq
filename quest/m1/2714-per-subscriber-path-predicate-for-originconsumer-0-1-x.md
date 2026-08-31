# [M] Per-subscriber path predicate for OriginConsumer (0.1.x) / AnnounceConsumer (0.2.x)

## Goal

Implement and verify the behavior tracked in [#2714](https://github.com/moq-dev/moq/issues/2714)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: answer the design question against
dev's reshaped announce/origin surface only; the 0.1.x backport half is
dropped unless a maintenance branch appears. See also the pattern-scoped
origin work at /quest/m2/path-patterns/origin.md.

### Issue context

Hi @kixelated 👋

We're building [WolvaneSfu](https://github.com/postanteGames/WolvaneSfu)  -  a MoQ-primary SFU backend (Rust media-engine + Elixir signaling) using `moq-net` for the relay side. We need a per-subscriber dynamic path exclusion filter to implement server-authoritative moderation (deafen enforcement  -  audio path subscribes filtered per-user based on server state) that a malicious/forked client cannot bypass.

Current `scope(&[Path])` is inclusion-only and immutable  -  it doesn't compose with a runtime-updated exclusion set. We'd like to add:

**Proposed API (0.1.x)**  -  `src/model/origin.rs::OriginConsumer`:

```rust
impl OriginConsumer {
    /// Returns a new consumer that drops paths from the announce stream when
    /// the predicate returns false. Unannounce events pass unconditionally
    /// (to preserve memory-cleanup semantics for previously announced paths).
    pub fn subscribe_filter<F>(self, predicate: F) -> OriginConsumer
    where F: FnMut(&Path) -> bool + Send + 'static;
}
```

Injection point: `OriginConsumerState::apply_announce`  -  predicate gate at the top.

**0.2.x equivalent** (`AnnounceConsumer`): a `with_filter(F)` chain builder after `Consumer::announced()`, applied inside `next() / poll_next()` drainage.

**Questions before I open a PR:**

1. Would you accept this API? Any alternative design you'd prefer (different predicate signature, or a fundamentally different approach)?
2. Should the PR target the `0.1.x` maintenance branch (we're pinned to 0.1.18) or `0.2.x` main? Happy to backport both directions if maintenance branches are still cut.
3. Is `0.1.x` still maintained, or should we be planning a `0.2.x` migration first?

If you'd rather I just open the PR and discuss inline, that works too  -  just wanted to check intent first since a "please migrate to 0.2.x" answer changes our plan meaningfully.

Test suite I'm planning (5 tests, matching the `#[tokio::test]` + `assert_next_wait` style in existing `origin.rs::tests`):

- `filter_blocks_matching_path`  -  paths where predicate returns false don't appear in `announced()`
- `filter_allows_unannounce_of_blocked`  -  unannounce passes unconditionally (memory cleanup)
- `filter_composable_with_scope`  -  `scope(&paths).subscribe_filter(pred)` chains correctly
- `filter_clone_independence`  -  filter is per-consumer, doesn't leak to siblings
- `filter_late_announce_gate`  -  announces arriving after subscribe still pass through the filter

Thanks  -  MoQ is finally starting to feel like it's ready for prod SFU work.

- Mustafa (WolvaneSfu)

## Closes

- [#2714](https://github.com/moq-dev/moq/issues/2714) - close this issue when the quest finishes
