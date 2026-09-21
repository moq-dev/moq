# [M] An extensible video group configuration

## Goal

The video API expresses group structure without an integer field that must be
replaced when intra refresh arrives. The current keyframe mode keeps working;
new backend modes remain deferred.

## Plan

Replace the integer gop with an extensible typed group configuration. Support
the existing keyframe interval first and refuse invalid intervals. Rename the
forced group-boundary request to cut consistently across direct codecs and
async sinks, preserving today's IDR behavior. Do not keep a compatibility alias.

Settle the rejection point before implementing this quest. Today keyframe()
only queues a request, and backend errors surface from encode(). The deferred
V4L2 refresh plan requires unsupported cuts to be refused, but that does not
by itself require a Result from cut(). Keeping the queued request infallible
and reporting rejection from the next encode is the simpler candidate; a
fallible cut would need a concrete reason for immediate acknowledgement.
The maintainer has not chosen between these contracts yet. Reconcile the
current V4L2 best-effort fallback with the chosen guarantee: it disables
unsupported force-keyframe requests and waits for a scheduled keyframe. Test
that case explicitly rather than assuming every backend honors the request.

Adapt capture, transcode's group-boundary cuts and eight-second backstop, and
in-tree binding implementations. Preserve externally published binding shapes.
The future refresh mode can add a variant and implement its different grouping
semantics without replacing the field or promising unsupported output today.
Do not depend on catalog warmup or an intra-refresh hardware backend here.

Tests cover a forced cut, buffered output, repeated group boundaries, invalid
configuration, and consistent direct/Sink behavior in CI. Update current GOP
documentation and leave the deferred refresh quest owning warmup/wire behavior.

Public API: typed GOP and cut naming in Rust video/transcode callers. Wire: no
change to current keyframe grouping.

## Related

- [Refresh groups](/quest/next/intra-refresh/encode-config.md) - later refresh variant and grouping implementation
