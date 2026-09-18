# [S] A fresh subscription over an idle link ramps for seconds

## Goal

A track re-spliced onto a source whose session carried nothing reaches the
publisher's rate within a round trip or two, instead of trickling for several
seconds and then bursting the backlog.

## Plan

Observed in the triangle testbed (`rs/moq-relay/tests/cluster_testbed.rs`):
after a route moves onto a relay whose link to the subscriber's relay was idle,
that link carries about 90 KB/s of a 340 KB/s stream for four to five seconds,
then 1.5 MB in one second, then the steady rate. The subscriber sees a p99
frame age of several hundred milliseconds from the switch alone, and the
relay expires the frames that fell more than the max age behind. The link in
question is loopback with a 60 ms one-way delay and no rate cap, so the shape
is the sender's, not the network's: an idle session's send-rate estimate is a
congestion window over an RTT, and whatever paces groups against it starts
from there.

Find what paces the first groups (the publisher's priority queue, the
congestion controller's slow start, or the PROBE-fed bandwidth grant), write a
test that measures the ramp on an idle in-process link, and either start from
the previous source's rate on a handover or let the controller open faster.

## Related

- [#2847](/quest/m2/2847-the-quinn-backends-send-bandwidth-estimate-is-cwnd-rtt.md) - the idle estimate is a window over an RTT on that backend too
