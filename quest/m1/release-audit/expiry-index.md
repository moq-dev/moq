# [M] Retire empty expiry deadline entries

## Goal

Completed or cancelled reads do not leave deadline metadata proportional to
historical demand on an otherwise live track. Live readers retain correct
wakeups without scanning the entire deadline table on every write.

## Plan

P2 resource concern introduced on main's expiry-index path, confirmed at
1d43c096e0. `rs/moq-net/src/model/expiry.rs` stores WaiterLists in media and
wall deadline maps. Waiter drop removes live registrations but not the keys.
Unlike the held/arrivals/starts indexes, these maps do not sweep empty lists.
In `track.rs`, Fetching::poll registers drift deadlines before returning a
cached hit; stalled group reads reach the same path. Distinct deadlines are
required: identical positions and budgets coalesce, and requested budgets
are clamped by publisher/cache policy.

An actual-source component harness registered and dropped 100, 1,000, and
10,000 waiters with distinct budgets of one second plus i microseconds.
Both maps retained exactly that many entries and zero live lists after
release_wall(now). This inspected the unchanged component with a private
probe, not a public subscriber integration test. Driven pools eventually
collect wall entries at their deadline. Wall expiry does not collect media
entries, which can remain on a quiet track until presentation advances,
metadata changes, or the track drops. Do not describe every cancelled read
as a permanent leak.

Tie empty-entry retirement to registration ownership or an amortized cleanup
mechanism. Preserve indexed wakeups. Add a public-model cancellation/cache-hit
test with mock time and a benchmark sweeping live readers and distinct
historical budgets, measuring retained entries and wakeup cost. No public API
or wire change is expected.
