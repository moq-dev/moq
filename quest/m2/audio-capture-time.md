# [S] Native audio stamps acquisition, not arrival

## Goal

Native audio capture stamps a buffer at the instant its first sample was
captured, like native video stamps a frame's acquisition. Today the driver
stamps the broadcast clock when it reads the buffer, so audio lands at least
one device buffer (plus callback and queue latency) later than video captured
at the same instant.

## Plan

cpal reports `InputCallbackInfo::timestamp().capture` per callback; carry it
through `capture::Samples` and map it onto the broadcast clock once per open,
the way video maps its private capture timeline. Fall back to arrival where a
host reports nothing usable, and keep the reset-on-gap behavior. Measure the
skew before and after on at least one real device, and extend the moq-audio
clock fixtures so a synthetic buffer's capture instant is what publishes.

Public API: none. Wire: none.
