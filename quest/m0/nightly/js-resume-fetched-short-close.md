# [M] A JS group continued by fetch keeps going past a failed route's short close

## Goal

The JS resume sweep passes `MOQ_RESUME_SEED=10` (4096 cases), which Rust passes:
a group continued onto a fetched copy that its route closes short and then fails
continues from the next live route, instead of ending where the failed route
closed it.

## Plan

Found on [moq-dev/moq#9410](https://github.com/moq-dev/moq/pull/9410), which
planned to fix it with the ended-reader seeds 4 and 7 but traced it to another
cause: the reader never ends. Shrunk:

```text
join r1 ordered none; ranges r1 [3,8]; up 0/1; write 0/0 g7 1; up 0/2;
down 0/0; close 0/2 down; write 0/0 next
```

`reader 1: group 7 ended after 1 frames where a failed route closed it`. The
reader reads frame 0 of group 7 from slot 0. Slot 0 dies, and
`Recover.#replace` (`js/net/src/resume.ts`) continues the group with a fetch
from slot 2, the serving route. Slot 2 closes it short at frame 1 and
fails; slot 1 is still live. Rust's `Recover::poll` waits for the next route on
a `Cause::Closed` whose copy failed. JS ends the group at frame 1.

Traced on that PR: in the model's `close 0/2 down`, the fetched group closes
short before slot 2 fails, and after that close nothing asks `Recover.wants`
about the group again, so slot 2's failure never reaches the group's
short-close judgment (`#continues` and `#readOn` in `js/net/src/group.ts`).
Decide from the Rust reader whether that judgment must revisit a close once the
copy fails, or whether the JS model's `close ... down` must let the failure land
before any read, as the Rust model's synchronous polling does. Fix it, with a
unit test, and add the case to the JS `regressions`.

Public API: none. Wire: none.
