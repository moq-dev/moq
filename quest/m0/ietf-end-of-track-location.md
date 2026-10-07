# [S] End of Track keeps its Location

## Goal

A relay forwards END_OF_TRACK at the Location its upstream sent, in
`rs/moq-net` and `js/net`. An upstream that ends the track at 5/5 on group
5's stream yields 5/5 on group 5's stream downstream, not 6/0 on a new
stream. A track whose last group stream has already closed still gets the
standalone marker at the next group's object 0, as today.

## Plan

Triaged from Fastly's follow-up on #5020 (runs of 2026-10-07 on `0f310a5`,
d18 and d21, cells `status-end-of-track` and `publish-done`). PUBLISH_DONE
was already right; only the marker moved. 6/0 is still true under d18's
"equal to or greater than" wording, but it drops the fact that group 5
ended after object 4, which a subscriber cannot learn any other way on
d16+ (END_OF_GROUP is gone).

Root cause, both sides:

- Ingress: `rs/moq-net/src/ietf/subscriber.rs` (`Ended::Track`, around line
  2688) finishes the group producer, then calls `end_track(group + 1)`. An
  egress reader that sees the group finish cannot yet know the track ends
  after it. Set the end first.
- Egress: `write_end_of_track` in `rs/moq-net/src/ietf/publisher.rs` always
  opens its own stream at `end`/0. When a group stream finishes and the
  track's `final_sequence()` is that group + 1, append END_OF_TRACK at
  (group, next object ID) before FIN, and skip the standalone marker for
  that subscription.

`js/net` mirrors both (`#runEndOfTrack` in `js/net/src/ietf/publisher.ts`);
check its ingress order too.

Test: a d18 subscriber stream that ends 5/0..5/4 then END_OF_TRACK at 5/5,
forwarded through the ietf publisher, reaches the downstream as 5/5 on the
group 5 stream, with no extra stream and a PUBLISH_DONE count to match.
A track finished after its last group closed still gets 6/0.

Public API: none. Wire: none (the marker's Location changes, its format
does not).
