# [S] moq-transcode: the retirement boundary regression test is a coin flip

## Goal

`retirement_finishes_an_in_flight_fetch` fails against a reverted fix on every
run and on any machine, instead of only when the scheduler happens to lose the
accept-vs-finish race.

## Plan

The test in `rs/moq-transcode/src/lib.rs` covers two ways a retiring rung can
cut an in-flight fetch short. It catches the first every time. The second is
finishing the track at a live edge sitting at or below the group the fetch is
about to claim, and that one depends on timing: `Consumer::fetch_group`
resolves as soon as the attempt is registered, well before `GroupRequest::accept`
creates the group, so which side of `accept` the retirement lands on is the
scheduler's choice. A loaded CI runner lost that race; a developer machine
never has. The test says so in its doc comment rather than pretending
otherwise.

Two constructions were tried in #3381 and both failed the negative control (a
test that passes against a reverted implementation is worth nothing), so the
fix landed resting on the structural argument instead. The attempts are on the
pull request thread.

What is missing is an observation point between a fetch registering and
claiming its group, which `rung.rs` does not have. Per no internal callbacks,
that is not a test hook parameter on `serve`. Prefer pulling the decision out
instead: the boundary question is "what sequence can `finish` take, given the
live edge and the fetches still in flight", and answering it in a function that
can be called directly makes the interesting case a plain unit test. Keep the
async test as coverage of the wiring around it.

## Related

- [Congestion-aware transcode ladders](/quest/m1/ladder/README.md) - the rung lifecycle this boundary belongs to
