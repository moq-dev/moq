# [M] An epochless track ends on an upstream error instead of re-splicing

## Goal

A delivered track whose front has no epoch ends with the upstream error
instead of re-subscribing. A re-subscribe through the same session can land on
a different instance one hop upstream. Only a front with an epoch re-splices
after an error, and its re-request carries that epoch, so it resumes the same
broadcast or fails. Consumers of an epochless broadcast recover on the announce
`Restart` (or `End` then `Start`), never on a silent re-splice.

## Plan

Found in #5052 (t0ms), decided 2026-10-09. On lite-06, a relay times out the
dead publisher's session and ends the downstream copy with `internal error`.
The downstream front takes `Front::track_ended`'s `Err(_) if delivered` arm
(`rs/moq-net/src/model/front.rs`), "normal failover", and `redispatch`es to
the same serving source, which is the relay session, about 2 ms later. The
relay resolves the epochless SUBSCRIBE to whatever wins the path now, the
replacement publisher, and the resume layer fetches the rest of the
in-flight group from it. The consumer sees no error and no restart. So two
epochless instances are stitched across a hop, against "routes without an
epoch never splice". #5087 does not change this: the redispatch arm is
untouched, and its held END/START reaches the downstream relay hundreds of ms
after the re-subscribe. In `moq export ts`, frames read before the death sat
in the decode hold through the silence and came out 13-26 s late, and the
export bailed on "missed a decode deadline".

- Decided: resume after an error only within an epoch. Rejected: a distinct
  terminal error from the relay for "upstream instance gone" (a new wire
  distinction every hop must classify), and accepting the stitch on lite-06.
- Accepted consequence: on lite-06, and on any epochless route, any upstream
  track error now ends the track. The epoch README already accepts that
  default sessions end subscriptions on failover instead of resuming them.
- Decided: no exporter-side guard. A unit due at or behind the TS schedule's
  `next` still bails. Once the stitch is gone, a stale unit reaching the
  schedule is a bug to fail loud on. The export drops the replaced broadcast's
  state through `ts::Export::follow`.
- Fix the module docs that claim an epochless front is never swapped
  (`front.rs` header, `origin.rs` around `pick`): today that holds only when
  the replacement is in the front's own table.
- Tests, with mocked time: an epochless publisher behind a relay is replaced
  (the old session times out, a new one is already announced) on lite-06 and
  lite-07. The downstream track ends with the error and never fetches a
  group from the replacement. A 1+1 pair sharing an epoch on lite-07 still
  fails over with no error downstream. The same pair reaching the relay on
  lite-06 ends the downstream track.
- After landing, ask t0ms to re-run both #5052 rigs. One rig-2 run exited
  1.4 s after a mid-GOP join, before any failover, which the stitch does not
  explain. If it reproduces, it is a separate issue.

Public API: none. Behavior: an epochless subscription now surfaces an
upstream error instead of healing. Wire: none.

## Closes

- [#5052](https://github.com/moq-dev/moq/issues/5052) - export ts bails after re-requesting onto a replacement publisher
