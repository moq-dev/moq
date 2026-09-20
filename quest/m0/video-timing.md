# [L] Capture timestamps and rational video rates

## Goal

Capture supplies a timestamped frame, and fractional frame rates survive native
capture, encoder configuration, and transcode without becoming integer fps.

## Plan

Capture currently returns only a Surface and publication timestamps it when
dequeued. Camera enumeration exposes a rational Rate, but requested/negotiated
capture and encode use integers; transcode rounds the catalog rate. Preserve
capture time before queueing and use one validated rational rate vocabulary
across these four crates.

Define the capture clock mapping and preserve source timestamps through resize,
encode, and buffering. Prefer native capture timestamps when available; record
the fallback timestamp at acquisition, never at dequeue. Do not expose unrelated
device clocks as if they were a shared broadcast epoch. Preserve audio/video
clock alignment in the capture publisher and keep unknown rates explicit.

Test 30000/1001 and 60000/1001, zero denominator, rate overflow, queued capture
delay, monotonic mapping, and transcode catalog/meter propagation without
hardware. Platform fixtures validate native timestamps separately. Keep wire
representations and already-published binding signatures compatible; converting
to their existing representations happens at that boundary with explicit rules.
Update capture/encoding docs and examples with the clock and rate units.

Public API: capture frame result and video/transcode rate types change. No wire
schema or binding layout change.
