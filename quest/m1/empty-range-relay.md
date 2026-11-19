# [M] A range answered empty upstream stalls a viewer behind a relay

## Goal

A viewer behind a relay whose subscription holds a lite-07 time range the publisher answers with no group to deliver (Range Start 0) still reads every later range.
Today, with a viewer subscribed through a relay to `Time(10)..Time(20)` and `Time(66)..` while the publisher holds only group 66, the viewer reads nothing: on `main` and with [moq-dev/moq#9172](https://github.com/moq-dev/moq/pull/9172) alike, the viewer's ordered read waits on a fill it asked at key 10 that never resolves, and the relay never answers the empty range as such.

## Plan

- Reproduce with the fixture in `rs/moq-net/tests/integration/waiting_time_start.rs` (publisher, relay, viewer): an unlimited-budget ordered reader with the two ranges above times out before reading 66.
- The cause is that an empty answer only holds under the `Subscriber Max Age` it was asked with (the draft's start "SHOULD start at the oldest group at or above the floor that Expiration has not expired"), so the copy can't place the time for every reader. [moq-dev/moq#9172](https://github.com/moq-dev/moq/pull/9172) records the answer (`TimeStart::start == None`) and only releases the ordered barrier for it; the time stays unplaced, so the relay's window for the downstream range waits, and a `Before` placement makes the copy look up history the publisher won't serve.
- Recommended: scope the empty answer by the budget it was asked under, so a reader whose budget fits places its time past the range's reach (the range resolves empty, and the relay answers 0 downstream), while a wider reader keeps looking it up. Mirror it in js/net.
- Also find why the viewer's fill at key 10 never resolves through the relay when the publisher's handler drops it: a dropped fill names its keys `NotFound`, which should give the fill up.
