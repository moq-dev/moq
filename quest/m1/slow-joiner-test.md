# [S] A slow joiner loses no groups within its budget

## Goal

A test drives a reader that stalls before its first read, over a transport
with real stream-credit backpressure, and shows the relay drops none of the
groups its budget covers.

## Plan

Found by [moq-dev/moq#6990](https://github.com/moq-dev/moq/pull/6990): a
live-edge audio join subscribed with only its decoder warmup as its budget,
so a C reader starved before its first read lost its queued groups at the
relay. That PR's unit test checks the subscribe budget, not the relay path.
Use moq-net's mock harness with a capped stream count and mock time.

Public API: none.
