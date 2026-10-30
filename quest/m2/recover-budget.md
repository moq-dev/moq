# [S] A parked read of a resumed group reads its budget without re-syncing

## Goal

A pending read of an open group handed out by a resumed reader checks its
expiry budget without locking the reader and re-running its route sync, so
relay fanout through fronts pays no per-frame, per-viewer sync. A benchmark
of resumed readers shows the per-read cost flat in the number of copies.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Source-inferred, not
benchmarked; new on main. Measure first, and delete this quest if the cost
does not show.

Every Pending read of an open group goes
`poll_expired_if_blocked` -> `poll_expiry` (`rs/moq-net/src/model/group.rs:2692`)
-> `Recover::poll_budget` (`model/resume.rs:1868`), which locks the
`Reader` and runs `sync`: `learn_end` over every copy (each taking its track
lock), `subscription.poll_ref`, and a route state read, only to read a
`Duration`. On a relay every downstream subscription to a front track goes
through this path.

Publish the reader's budget through the existing preferences channel (or an
atomic) for `Recover` to read, and keep `sync` on the reader's own poll path.

Public API: none. Wire: none.

## Related

- [Resume as producer replacement](/quest/m2/resume-redesign.md) - deletes this path entirely
