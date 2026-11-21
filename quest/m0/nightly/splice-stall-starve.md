# [M] The ring6 mesh replay starves no reader under four CPUs

## Goal

`taskset -c 0-3 just test mesh-replay --mesh test/mesh-replay/ring6.toml --broadcasts 32`,
with release-built relays, starves no reader, so Nightly's mesh replay stays
green now that no `[[known]]` entry covers a stranded or starved reader.

Non-goal: the same run on two CPUs, unless the four-CPU run passes and the
two-CPU starvation turns out to be the same cause.

## Plan

Found while working [mesh-replay-violations](https://github.com/moq-dev/moq/pull/9236),
on `main` at `9828a20cab`, before
[moq-dev/moq#9231](https://github.com/moq-dev/moq/pull/9231) landed
([evidence](https://github.com/moq-dev/moq/pull/9231#issuecomment-7049596622)):

- 4 CPUs: `reopen every link of b (1/2)` starved 21 readers (`d` on
  `replay/b/*`, 75s).
- 2 CPUs: after `kill and restart b`, reader `a` read nothing on `replay/d/*`
  and `replay/e/*` for 150s while `replay/c/0` on the same relay read 10
  groups/s.

\#9231 finished the splice-stall quest (a lite-07 relay holds a request for a
claim it shows available) and dropped its `[[known]]` entries, so nothing owns
this starvation if it survives that fix.

Decided 2026-11-20 (maintainer, `/quest-plan` after a `/quest-run`): ✅ rerun
first, then fix (rejected: include the two-CPU case in the goal; plan nothing
and let the Nightly triage decide).

- Rerun the recipe on `main` once #9231 and #9236 (release-built replay
  relays) have both landed, three times. If no reader starves, delete this
  quest in a PR that records the runs.
- Otherwise find which relay holds the starved readers' tracks and why it
  stops feeding them, from the replay's relay and reader logs, and fix the
  cause in moq-net. A starved reader that reads nothing while its relay still
  shows the broadcast is the same failure the deleted splice-stall quest
  described (`git show 1a34abe437:quest/m2/multicast/splice-stall.md`).
- CPU pressure is how the bug shows itself, not the cause: no new `[[known]]`
  entry for load, and no longer deadline.

Caused by: unknown until reproduced; check whether #9231 is the last change
to the starved path before blaming it.

Public API: none expected. Wire: none expected.
