# [S] moq-transcode: the retirement boundary regression test is a coin flip

## Goal

`retirement_finishes_an_in_flight_fetch` fails against a reverted fix on every
run and on any machine. Getting there means first establishing the interleaving
that fails, because the two mechanisms written down so far are both ruled out
by the code.

## Plan

What is known: the test in `rs/moq-transcode/src/lib.rs` caught the boundary
bug on a loaded CI runner against the un-fixed implementation, and has never
failed on a developer machine. Two constructions for determinism were tried
in #3381 and both passed against the reverted fix, which is a failed negative
control, so the fix landed resting on its structural argument instead. The
attempts are on that pull request thread.

What is not known is why. Two candidate mechanisms are already out:

- The test's doc comment says `Consumer::fetch_group` resolves as soon as the
  attempt is registered, well before `GroupRequest::accept` creates the group.
  It does not. `Fetching::poll` resolves through `TrackState::poll_fetch_cached`,
  which is ready only once the sequence is in the track's lookup, and `accept`
  is what puts it there (the other exits are an abort, past-final, and a
  written rejection). The awaited fetch has already seen the group accepted, so
  retirement cannot land on the far side of `accept`.
- `rung::serve` says a `finish` racing the accepted group can refuse it,
  because the live edge on a rung that only ever served fetches is sequence 0.
  It cannot. `accept` goes through `insert_group_request` into `insert_group`,
  which advances `max_sequence` whether or not the group is visible, so
  `Producer::finish` takes the exclusive boundary 1 and leaves group 0 alone.

Both comments are wrong on main and get corrected by this quest whatever it
concludes.

The candidate left is cancellation: before #3381, `serve` selected over `live`
and `fetches`, so `live` returning on retirement dropped the `fetches` task,
and with it a `group::Producer` that had been accepted but not finished. Note
that reverting the fix reverts the retire signal itself, so state precisely
what the baseline is before drawing conclusions from it.

Establish the interleaving empirically, on a loaded runner and with
instrumentation, rather than reasoning out a third construction from the code.
The two entries above are what that reasoning has produced so far. Only once
the ordering is known is it worth deciding what to make directly testable; per
no internal callbacks, whatever that is, it is not a test hook parameter on
`serve`.

## Related

- [Congestion-aware transcode ladders](/quest/m1/ladder/README.md) - the rung lifecycle this boundary belongs to
