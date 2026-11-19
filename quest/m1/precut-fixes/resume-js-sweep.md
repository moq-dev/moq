# [L] Nightly's JS resume sweep passes

## Goal

`MOQ_RESUME_SEED=1 MOQ_RESUME_CASES=50000` and seed 7 pass on
`js/net/src/resume.model.test.ts`, so Nightly's `test/resume-model.sh` passes
on its JS side. Each defect behind a failure is fixed at its root, matching the
Rust reader (`rs/moq-net/src/model/resume.rs`) where Rust passes the same case.

## Plan

The fix for this sweep's `group N outside ranges` failures let it run past
where `main` failed, uncovering the defects below. Both fail the same way on
`main` at [f1df3c3a8e](https://github.com/moq-dev/moq/commit/f1df3c3a8e).
Seeds 3, 10, 11 and 12 pass 50k cases; seeds 1, 2, 4, 5, 6, 7, 8, 9 and 13
still fail. Expect more behind these: each fix so far exposed the next.

Replay a counterexample by adding it to the `regressions` test.

1. **Widened local bounds lose a group.** Rust passes this case.
   Seed 1 at 11,325 cases:

   ```
   join r3 ordered [0,1]; bounds r3 [1,2]; write 0/0 next;
   bounds r3 none; read r0
   ```

   This fails with `missing groups 0`. Untraced; the likely cause is that the
   ordered cursor passes group 0 while the bounds start at 1, and lowering them
   asks no backfill because the serving copy has no resume point (`#backfill`
   returns without one). Seeds 2, 4, 5, 8, 9 and 13 fail with
   `missing groups` in cases that also move local bounds. Their counterexamples
   are in the sweep output; check each against Rust.
2. **A forgotten drop accounts a group never written.** Rust passes this case.
   Seed 7 at 1,450 cases:

   ```
   up 1/1; giveUp 1/1 g2; expire; up 1/2; join r0 unbounded;
   write 1/1 next; write 1/2 g2; down 1/2; leave r1
   ```

   This fails with `missing groups 1`. Route 1/1's drop of group 2 expires once
   the route's edge passes it, and `#forgot` (Rust `forget_drops`) accounts
   `[1, 3)` as arrived, "the gap below it too, given up early as a fold would",
   so the backfill copy is let go before group 1 lands. Rust's model never
   forgets that drop because its pool tick (`Pool::now`) moves only when the
   pool driver polls, while JS's `Pool.now()` reads the clock on demand. Rust in
   production polls its pool and may share the defect. Seed 6 fails the same
   way at 16,450 cases, also a give-up followed by `expire`.

   Decided 2026-11-19 (coordinator, maintainer away, merge granted): decide by
   test. Port this expired-drop counterexample (seeds 6 and 7) to a Rust test
   driven by a clock that advances only when polled, the way JS reads it. If
   Rust fails, the drop rule is wrong: fix it in both languages. If Rust
   passes, fix how JS reads its clock.

3. **`Copy.dropped` has no Rust counterpart.** A group the JS window drops
   raises its copy's resume point (`resumePoint`), so widening backfills it;
   Rust's `poll_lowest` drops it and its `backfill`/`backfill_gaps` read only
   `copy.floor`. Check whether a Rust serving copy with no floor can offer a
   cached group below a window a replaced copy placed. If it can, Rust loses
   that group on widening: port the mechanism. If not, write down on
   `Copy.dropped` why JS reaches that shape and Rust does not. Raised by
   Pullfrog on [moq-dev/moq#9115](https://github.com/moq-dev/moq/pull/9115).

Nightly's seed 1218508351 (run
[44342367698](https://github.com/moq-dev/moq/actions/runs/44342367698)), which
`quest/m0/nightly/js-resume-window.md` tracked, passes 4096 cases since
[moq-dev/moq#9115](https://github.com/moq-dev/moq/pull/9115). That quest's
partial branch `fix/js-resume-window-wip` (`0f1b73d365`) is superseded by
#9115's window port and holds nothing more; delete it when this quest lands.

Public API: none. Wire: none.
