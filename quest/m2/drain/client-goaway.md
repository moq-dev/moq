# [L] Client goaway

## Goal

Reconnect native and JavaScript clients through fresh DNS after GOAWAY.
Preserve the app-visible session and avoid pinned addresses; today both
clients only decode and log GOAWAY.

## Plan

moq.pro's (downstream) fleet drain orchestration relies on this behavior: a
drained node is already out of DNS when GOAWAY fires, so a re-resolve is what
lands clients on a healthy relay.
