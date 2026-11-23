# [M] A JS group continued onto a shorter failed copy never ends clean where no copy closed it

## Goal

The JS resume sweep passes `MOQ_RESUME_SEED=9` (4096 cases), which Rust passes
and which passes on `main` too: a group read across failed route copies never ends
cleanly at a frame count no copy closed it at.

## Plan

Found on [moq-dev/moq#9396](https://github.com/moq-dev/moq/pull/9396), once the
JS model's routes became received copies. Shrunk:

```text
write 0/0 next 1; write 0/0 g3 1; write 0/0 next 2; join r0 unordered none;
up 0/1; write 0/0 next 1; write 0/0 next 1; close 0/1 down; down 0/0;
ranges r0 none
```

`reader 0: group 2 ended cleanly after 2 frames`. Group 2 has 3 frames. Slot 1
fetched it with 1 frame and closed it short there, and slot 0 held 2 frames,
still open, when its route failed. Traced on that PR: the read reaches frame 2
on one copy, which then fails. `Recover.#replace` (`js/net/src/resume.ts`)
continues it from a failed replaced copy through `cachedGroup` ("a failed group
continues from any copy still holding it"). That copy holds no frame at index
2, so the read reaches its close. The closed-cause replace then finds no
serving source and throws `standing()`, and `#resume` turns that into a clean
`#end()` at frame 2.

Decide from the Rust reader (`Recover::held` and `poll`) which step is wrong.
Either a copy that cannot reach the reader's frame must not take the group, or
the group must end with the route's failure instead of cleanly. Fix it, with a
unit test, and add the case to the JS `regressions`.

Public API: none. Wire: none.
