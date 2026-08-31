# [M] Relay drain api

## Goal

Expose a drain hook that sends GOAWAY on every relay session. Include new
arrivals while draining, so an embedding process can trigger it on SIGTERM.

## Plan

moq.pro's (downstream) fleet drain orchestration consumes this hook: its edge
process sheds the node from GeoDNS, waits out the TTL, then fires the drain.
