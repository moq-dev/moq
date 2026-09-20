# [S] Expose audio publication authority without its internals

## Goal

Audio callers can observe demand without bypassing packetization, and the
public surface stops exposing unused resampler mechanics and duplicated
capture-construction arguments.

## Plan

Replace Producer::track with demand, matching video. A shared transport Producer
can append or finish outside the audio publisher's framing and padding rules;
the external consumers inspected only need its demand handle. Retain privileged
access privately for capture ownership where necessary.

Make the standalone Resampler private: no in-tree external consumer needs its
window/timestamp machinery, and its corresponding held-at accessor is already
private. Let publish_capture take the existing PublicationOptions instead of
reconstructing it from five arguments. Reserve extension points on Frame and
Encoded records through their constructors before adding future metadata.

Preserve arbitrary-sized PCM writes, priming, terminal padding, and the
intentional ability to abort after finish. Adapt bindings and examples without
changing their public signatures. Test demand lifecycle and publication close
behavior through the supported API in CI; update crate examples and rustdoc.

Public API: remove privileged/helper exports and simplify capture arguments.
Wire: none.
