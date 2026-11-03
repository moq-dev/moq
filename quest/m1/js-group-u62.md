# [S] @moq/net group IDs past 2^53

## Goal

`@moq/net` decodes, holds, and re-encodes every group ID the wire allows (a
62-bit varint) on lite and moq-transport, instead of refusing any past 2^53.
A track whose group IDs are large, such as start ticks on a fine timescale
or a third-party moq-transport publisher's IDs, plays and relays through JS.

## Plan

Decided 2026-11-03 (maintainer, the `/quest-import` grill of
[moq-dev/moq#7573](https://github.com/moq-dev/moq/issues/7573)): "JS should
fix that regardless". Legacy group IDs now pass through verbatim as keys
([the model](/quest/m1/time-addressing/model.md)), so JS can no longer assume
they stay small. This replaces
[@moq/net by start](/quest/m1/time-addressing/js.md)'s plan to refuse a start
past 2^53.

Today `js/net/src/lite/group.ts` reads and writes the sequence with `u53`,
and the moq-transport codecs do the same. Carry group IDs as `bigint` at the
wire and in the model, or add a checked u62 type if `bigint` costs too much
on the hot path. Measure both with the JS benches and pick the simpler one
that holds. The API breaks where a group ID is a `number` today. Test the
decode and encode of 2^53 + 1 and 2^62 - 1 on every wire version, through a
JS relay hop.

Public API: breaking in `@moq/net` where group IDs are numbers. Wire: none.
