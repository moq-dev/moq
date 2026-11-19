# [M] A front keeps delivering after it switches copies on lite-07

## Goal

On lite-07, a relay's front that switches to a new copy after a link reopens
or a relay crashes keeps delivering every group to its readers, as the
release (lite-06) does. Today it can deliver for a few seconds, then hand
out one group every 15-25s for a minute or more, with its route unchanged.

No reader starves in
`just test mesh-replay --mesh test/mesh-replay/ring4.toml --script test/mesh-replay/reopen.toml --broadcasts 32`
or in the nightly ring meshes, once `[[known]]` no longer names this quest in
`test/mesh-replay/script.toml`.

## Plan

Found by [the mesh replay](/test/mesh-replay/README.md) while fixing
[reopen-starve](https://github.com/moq-dev/moq/pull/9179), whose fix leaves no
request refused and never asked again. With that fix, 9 of 13 `reopen.toml`
runs on ring4 at 32 broadcasts passed (2 of 3 with
[#9154](https://github.com/moq-dev/moq/pull/9154)), and the others starved 3
to 205 reader paths. Before it, 17 of 24 passed, and failures starved 1 to 32
paths, some of them stranded requests.

What an instrumented failing run shows, after every link of `a` reopened:

- `a`'s front on `b/0` switched to the direct route to `b` as `b`'s copy of
  the claims caught up, spliced the new copy, and fed its reader for 13s.
  From then on its route never changed and no request was refused, yet
  `b`'s groups reached `a`'s reader once every 15-25s.
- Over the same minute, `d`'s reader stalled the same way on all of `c`'s
  broadcasts, through a front on the `c`-`d` link that the fault never
  touched and that never switched.
- No shaper dropped or overflowed a packet, and every reader's status kept
  time.

Pre-fix runs showed the same signature on a handful of paths: a front
spliced a copy that delivered, then its reader got nothing for 57-75s.

Likely cause, untested: [roll-echo-stall](https://github.com/moq-dev/moq/pull/9174)'s
first fix. A shared lite-07 copy never places a time start that a wider
range already covers, so the subscription it feeds waits. A spliced copy
resumes from a time. Its head could not be tested here: it no longer builds
on main, and its merge is being redone. Once it lands, rerun `reopen.toml`
several times. If no reader starves, delete this quest and its `[[known]]`
entry. Otherwise, instrument the stalled subscription on both ends of the
link: which groups the publisher serves, and what the subscriber's window
waits on.

The lite-07 cut waits on it, as decided for reopen-starve on 2026-11-19
(under the maintainer's delegation for lite-07 routing calls): it is the
same starved reader after a link flap or a crash, which a deployment sees
daily.

Not a wire change.

## Related

- [reopen-starve](https://github.com/moq-dev/moq/pull/9179) - a request
  refused before the neighbour's claims returned, and never asked again
- [roll-echo-stall](https://github.com/moq-dev/moq/pull/9174) - a time start a
  shared copy never places
