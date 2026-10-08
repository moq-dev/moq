# [S] Media late join catches up to live

## Goal

A viewer joining a broadcast through a relay after a demand gap shows video
promptly and reaches the live edge within a bound, and the late-join check
asserts exactly that and passes under load.

## Plan

It failed once after [#4181](https://github.com/moq-dev/moq/pull/4181) ("16
frames behind" a 15-frame GOP), and the interop browser check failed the
same way on `main`. `just test media` is that browser lane through a Rust
`moq-relay`; there is no separate Rust media test.
[#4719](https://github.com/moq-dev/moq/pull/4719) now compares the
latecomer against the published keyframe instead of the painted counter.

It is real, not fixture sampling: the latecomer is handed a GOP from before
the demand gap. Reproduced by looping `bun media.ts --cases late-join`
against a local relay with debug logs. In the fixture, the old viewer
leaves, the publisher pauses (closing its open GOP and writing an empty
marker group), and the latecomer joins about a second later.

[#4914](https://github.com/moq-dev/moq/pull/4914) fixed two relay windows,
each with a deterministic test:

- A received group stays hidden from readers until its first frame lands or
  its stream ends, and an idle copy goes live only once the cache shows the
  route's answer (`rejoin_waits_for_the_answers_first_frame`).
- A rejoin's answer is judged against the groups that survived the leave,
  not the idle snapshot, since the cancel resets the group in flight
  (`an_answer_past_a_reset_group_skips_the_cache`).

What remains (1 of 30 runs after both fixes): the latecomer's max delay rises
to its playout delay (about 145ms) right after it subscribes. When the
pre-gap GOP was short, the GOP before it still reaches within that budget of
the marker, so the relay hands it over by design. The player then presents
from that GOP's keyframe, not from its playout position. Relay log: groups
7, 8, 9 served in 1ms with a 145ms budget, and the player shows frames 360ms
behind the published keyframe.

Decided 2026-10-06 by the maintainer: a joiner showing the reachable pre-gap
GOP at once and then fast-forwarding to live is the wanted behavior, so no
player or consumer change. The check is what's too strict: it asserts the
first presented frame is at or after the newest published keyframe.

Remaining work: the late-join check (`just test media` and the interop
browser lane) asserts what a viewer cares about instead: video shows within a
start bound, and the player reaches the live edge within a catch-up bound.
Set both bounds from measurements, looped under synthetic CPU load (the
maintainer approved a bounded load generator such as `stress-ng` for these
runs), and keep the relay fixes' deterministic tests as the regression guard.

Public API: none. Wire: none.
