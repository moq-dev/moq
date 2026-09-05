# [S] moq-transcode: the retirement boundary regression test is a coin flip

## Goal

`retirement_finishes_an_in_flight_fetch` fails against a reverted fix on every
run and on any machine. Getting there means first establishing why it does not
today, because the ordering its doc comment names is not one the code can
produce.

## Plan

What is known: the test in `rs/moq-transcode/src/lib.rs` caught the boundary
bug on a loaded CI runner against the un-fixed implementation, and has never
failed on a developer machine. Two constructions for determinism were tried in
#3381 and both passed against the reverted fix, which is a failed negative
control, so the fix landed resting on its structural argument instead. The
attempts are on that pull request thread.

What is not known is the interleaving that actually fails, and the doc comment
on the test asserts one the code rules out. It says `Consumer::fetch_group`
resolves as soon as the attempt is registered, well before `GroupRequest::accept`
creates the group. It does not: `Fetching::poll` resolves through
`TrackState::poll_fetch_cached`, which is ready only once the sequence is in
the track's lookup, and `accept` is what puts it there (the other exits are an
abort, past-final, and a written rejection). So the `fetch_group(0, None).await`
in the test has already seen the group accepted, and retirement cannot land on
the far side of `accept`. Correct that comment whatever else this quest
concludes.

The candidate the reshape of `serve` was built around is still open: `finish`
takes the live edge, which on a rung that only ever served fetches is sequence
0, and a group at or above the boundary is refused, so a `finish` racing the
accepted group's completion is what cuts it short. Establish that on a loaded
runner against the un-fixed implementation, by instrumentation rather than by
guessing at a third construction.

Once the ordering is known, prefer making the decision unit-testable over
adding an observation point to `serve`: the question is what sequence `finish`
may take given the live edge and the fetches still in flight, and answering it
in a function that can be called directly makes the interesting case a plain
test. Per no internal callbacks, a test hook parameter on `serve` is not the
answer.

## Related

- [Congestion-aware transcode ladders](/quest/m1/ladder/README.md) - the rung lifecycle this boundary belongs to
