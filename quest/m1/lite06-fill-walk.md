# [L] A lite-06 relay copy fetches a timed track's skipped keys one by one

## Goal

On moq-lite-06, a late subscriber that joins a timestamp-keyed track through a
relay (`moq import fmp4` → moq-relay → `moq-hls` export) gets its first
segments as quickly as it does on moq-lite-07. The relay copy settles the keys
between two group starts without one FETCH per key, and every answer about
them still comes from upstream.

## Plan

Seen as a downstream recorder that joins a relay copy about 4 s after a
looped clip starts and stores no segment for 45 s, from either of 2
renditions. It fails in some runs on an idle host and more often on a loaded
one. It is a real bug: load only makes the late join, and the resets that
start it, more likely.

From a captured run (lite-06 between publisher and relay):

1. In its first 4 s the publisher reset 5 group streams as `Old`: video 0,
   1000000 and 2000000, audio 1021678 and 2020136. The relay logged
   `group error err=old` for each.
2. Right after, the relay's copy began fetching the video track's keys one at
   a time, downward from 2999999, each answered `not found`. It then walked
   the span after every later video group too (3999110, 4999059, through
   11023242), though none of those groups was reset: 11,421 fetches in 42 s.
   The export's ordered reader waits behind the fill, so the first segment
   never lists.

So the unaccounted keys are not just a reset group's span. A finished group's
span is already named: `settle_failed` and `shed` queue a SUBSCRIBE_DROP over
`span(key, group.end())` on lite-06 too (`has_track_stream()` is true from
Lite05). lite-06 carries no GROUP_END (`has_group_end`) and declares no skipped
starts (`has_skip`, `has_group_span`), so the keys inside every group stay
unproven in the copy. A reader that wants them, here apparently once the
resets broke its ordered read, turns them into misses after `ledger::GRACE`
(1 s). `FillServeRun` (`rs/moq-net/src/lite/subscriber.rs`) then sends one
FETCH per wanted key, which a microsecond keyspace turns into a million round
trips per second of media.

An in-process reproduction with the mock transport (late reader, idle-cancel
and resubscribe, bursts; drafted against `rs/moq-net/src/lite/`) never made
the publisher reset an opened stream and passed on both an older pin and
current `main`. The first step is a test that fails on `main`. It forces
`Old` resets of opened, serving group streams early in a lite-06 subscription
to a timestamp-keyed track, after their headers went out: a group expiring
under the subscription's budget while the transport parks its stream
(`poll_given_up`), or an injected reset. `give_way` will not do, since it
gives up the group waiting for stream credit before it has a stream. Then it
attaches an ordered reader from the start. It asserts the reader reaches later
groups and the copy sends a bounded number of FETCHes. That test also pins
down which reader wants the interior keys, and why a reset makes it want them.

Recommended, not decided:

- **Relay copy**: upstream stays the authority on unseen keys (a relay that
  joined live cannot prove a key absent from what it never saw), but a lite ≤06
  fill over a run of unproven keys asks once per run, not once per key. Two
  options, to choose once the test exists:
  - One request over the run. A lite-06 SUBSCRIBE from the run's start lets
    the publisher's SUBSCRIBE_DROP name its empty span, which the copy accounts
    as the publisher's answer. Preferred if the test shows the copy can bound
    and cancel that subscription.
  - FETCH only the keys of the run that have evidence of a group (an arrived
    header, a cache or eviction record, or a SUBSCRIBE_DROP name), and ask
    upstream about the rest in one request as above.
- **Reader**: if the test shows the ordered reader wants a lite-06 group's
  interior keys only because a reset broke its read, fix that read too, so a
  reset group costs at most its own key.
- Rejected: capping fetches per fill (an arbitrary number that still stalls a
  reader for seconds), answering unseen keys `not found` locally (the copy
  would hide content upstream still holds), and a publisher-only fix (it
  already names finished spans, and a third-party lite-06 publisher would
  still trigger the walk).
