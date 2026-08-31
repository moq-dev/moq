# [M] Nonblocking SEI delivery

## Goal

A live stitcher receives sidecar SEI before the matching video access unit
without relying on cross-track publish order or extending the video's existing
release deadline. If no such mechanism holds under real transport loss and
reordering, the verdict is no default separation and SEI remains in-band.

## Plan

Prototype the smallest cross-track delivery primitive that can satisfy the
contract. Candidate shapes include an explicit object dependency understood by
the sender scheduler, a cache-addressed sidecar reference that can be resolved
before video delivery, or another mechanism demonstrated against both moq-lite
and IETF MoQ. Track priority and `Subscription::ordered` alone do not qualify:
they are scheduling preferences, not receive-order guarantees.

The video framing says whether sidecar data exists and identifies it by group
sequence and frame ordinal. The consumer emits immediately when the marker says
none, and otherwise uses only the time already available in the video jitter or
latency budget. A lost sidecar cannot move that deadline. Make loss observable
rather than silently claiming a byte-faithful stitch.

Test normal multiplexing, packet reordering, sidecar stream loss, video stream
loss, congestion, relay fan-out, late join, reconnect, and a subscriber that
does not request SEI. Measure added video latency and bandwidth. Accept the
mechanism only if video remains within its existing latency budget and every
delivered sidecar is associated without timestamp equality.

Record a go/no-go verdict. A no-go removes or rewrites the dependent stitch
and default-separation quests rather than weakening the nonblocking boundary.

## Required

- [SEI catalog section](/quest/m2/sei/sei.md) - defines the identity and
  presence signal whose delivery this quest must prove

## Related

- [Rust SEI split and stitch](/quest/m2/sei/sei-rust.md) - its stitch path
  waits on this verdict
- [Web SEI stitch](/quest/m2/sei/sei-web.md) - likewise gated by this verdict
