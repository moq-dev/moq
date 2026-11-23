# [M] A JS resumed reader owes nothing below a time start the local floor keeps out

## Goal

When the local floor keeps out the group that holds a window's time start,
a JS resumed reader (`js/net/src/resume.ts`) owes, and asks the serving route
for, only the groups from that start. One cursor and the Rust reader already
behave this way. Today JS reads such a window as starting at group 0, so it
asks for, and waits on, groups below the window. If the route then answers for
the window's own groups, the read can stay pending where one cursor's ends.

## Plan

Found on [moq-dev/moq#9435](https://github.com/moq-dev/moq/pull/9435), while
writing its test "a window moved down under the cursor keeps one ask".
To reproduce it:

1. Route `a` writes groups 10 and 11, and a front serves it.
2. A reader subscribes to time `10..=13` with an hour's budget, and reads 10
   and 11.
3. The front serves route `b`, which holds groups 0 to 9 and 12. The reader
   sets its local range to `10..13` and reads 12.
4. The reader's window moves to time `9..=13`.

JS then asks `b` for `[0,10)`, where Rust asks for `[9,10)`. If the local floor
admits group 9 instead, JS hands out only group 9. So only the kept-out
judgement places the start wrong.

The likely cause: `Reader.exhausted` builds its gaps from
`window.floor() ?? 0n`, and `#asking` reads the window the same way. The start
here is one the serving copy's floor raised, which only a backfill places. That
backfill reads the group holding the time only once the local floor lets it,
so the start stays unplaced, and `?? 0n` reads it as group 0.

Recommended: place that start from the group holding its time before judging
the keys the local floor keeps out, as Rust does. While nothing has placed it,
treat the window as pending, not as starting at group 0. Add a unit test in
JS, and its Rust mirror, where the route's answer for the window's own groups
ends one cursor's read and must end the resumed read too.
