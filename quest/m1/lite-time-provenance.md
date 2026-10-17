# [M] Received timestamps are media time

## Goal

A receiver can trust a received timed track's timestamps as media time, so
the rewind check that today runs only on a local publisher's writes can run
across hops too.

## Plan

Split from [#5803](https://github.com/moq-dev/moq/pull/5803), which refuses a
rewind only on a local publisher's writes: a relay hop that checked received
groups closed tracks whose timestamps were never media time.

Where non-media timestamps come from today:

- Released clients stamp wall-clock write time on data tracks: the moq-mux
  catalog, moq-json, moq-stats, moq-room chat, js/json, js/binary, and
  js/net's `writeString`, `writeJson` and `writeBool`.
- Released relays stamp arrival time on input that carries no timestamps
  (IETF, and lite-04 or older) and forward it as media time.
- Unreleased main stamps send time on untimed tracks over lite-05/06, which
  cannot encode an untimed track
  ([#4822](https://github.com/moq-dev/moq/pull/4822),
  [#4968](https://github.com/moq-dev/moq/pull/4968)).
- Media tracks carry real presentation times.

Options weighed:

1. Fix our unreleased publishers and relays so only media time reaches the
   wire, and stop stamping data tracks. Recommended.
2. Reserve an untimed marker value on lite-05/06.
3. Treat a 1000 timescale as untimed.
4. Distrust every lite-05/06 timestamp.
5. A lite-07 TRACK_INFO marker for timestamp provenance.

Released peers still send wall-clock stamps, so whichever option lands must
say which received tracks the check may trust.
