# [L] Nightly's JS resume sweep passes

## Goal

`MOQ_RESUME_SEED=1 MOQ_RESUME_CASES=50000` and seed 7 pass on
`js/net/src/resume.model.test.ts`, so Nightly's `test/resume-model.sh` passes
on its JS side. Each defect behind a failure is fixed at its root, matching the
Rust reader (`rs/moq-net/src/model/resume.rs`) where Rust passes the same case.

## Plan

The fix for this sweep's `group N outside ranges` failures let it run past
where `main` failed, uncovering the defects below. Items 1 and 2 fail the same
way on `main` at [f1df3c3a8e](https://github.com/moq-dev/moq/commit/f1df3c3a8e).
Item 3 is that fix's own gap: `main` hands the group out early, outside the
window, and fails `outside ranges` instead. Seeds 3, 11 and 12 pass 50k cases;
seeds 1, 2, 4, 5, 6, 7, 8, 9, 10 and 13 still fail. Expect more behind these:
each fix so far exposed the next.

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
   pool driver polls, while JS's `Pool.now()` reads the clock on demand. Decide
   whether the forget rule or the JS clock is wrong: Rust in production polls
   its pool and may share the defect. Seed 6 fails the same way at 16,450
   cases, also a give-up followed by `expire`.
3. **A group the window dropped is lost once the ranges widen.** Seed 10 at
   79 cases:

   ```
   up 1/0; join r0 [3,4]; up 1/2; write 1/0 g3; write 1/2 next;
   down 1/0; ranges r0 none; leave r1
   ```

   This fails with `missing groups 0`. JS switches to route 1/2 before any
   group is handed out, so its copy has no resume point. That copy places time
   3 at group 0 (the only group it holds), and the window drops group 0 by
   taking it, as Rust's `poll_lowest` does with `sub.take`. Widening the ranges
   then has no resume point to backfill under. Rust passes only because its
   front switches after handing out group 3. A dropped group must stay
   reachable once a widened window names it: leave it unread on the copy, or
   backfill below the dropped key.

Public API: none. Wire: none.

## Related

- [Resume window mutants](/quest/m0/nightly/resume-window-mutants.md) - the same nightly's Rust side
