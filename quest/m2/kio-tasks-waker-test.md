# [XS] kio::Tasks tolerates a removed task's waker firing into its reused slot

## Goal

A unit test in `rs/kio/src/task.rs` shows that when a task taken out with
`Tasks::remove` wakes after another task was `insert`ed into its slot, the
new occupant is polled at most once spuriously, still finishes on its own
wake (handed back through `poll_each`), and its `TaskKey` still removes
it.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7340](https://github.com/moq-dev/moq/pull/7340), whose
online review suggested it.

[moq-dev/moq#7340](https://github.com/moq-dev/moq/pull/7340) added `insert`, `remove`, `TaskKey`'s generation stamp, and
`poll_each`. The existing
`a_stale_wake_costs_at_most_one_spurious_poll` (`task.rs:754`) covers a
*finished* occupant's waker through `push`, and
`removed_tasks_free_their_slot_and_key` (`:560`) opens the removed task's
gate only while its slot is empty. Neither fires a removed task's waker
after the slot is reused, which is the path the lite subscriber's
`Change::Close` takes. Write that case with the `gated` helper and the
removed task's leftover slot waker, as the stale-wake test does.

Not a quest: [moq-dev/moq#7340](https://github.com/moq-dev/moq/pull/7340) also noted that no bench sweeps lazy discovery with many
prefixes (its viewers use `client_discovers_all`). Add a lazy axis to the
session bench only if a later change to announce discovery needs it.

Public API: none. Wire: none.
