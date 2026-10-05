# [M] moq-transport ranges

## Goal

A relay fills past-range misses from a moq-transport upstream with one
standalone FETCH per run of locally missing groups, never including the
upstream's live group, so a sparse track costs a handful of requests. It
serves a downstream IETF FETCH from the model's ranges without blocking on the
live group.

## Plan

This absorbs the relay half of fetch-span (#4558) and builds on fetch-fill's
checks (#4544). Receiving a multi-group FETCH stream means several groups on
one stream. Absent sequences between the FETCH's groups become drops, and a
group already cached is discarded as a duplicate. Cap a downstream FETCH at
the Largest Object, as the moq-transport drafts require.

## Required

- [Model ranges](/quest/m1/subscribe-ranges/model.md) - the range requests this answers
