# [S] moq-dev-js receives objects from imquic

## Goal

The moq-dev-js runner client passes `data-subgroup-basic` against imquic
(draft-22 over WebTransport), as moq-dev-rs already does.

## Plan

Found while working
[moq-dev/moq#6275](https://github.com/moq-dev/moq/pull/6275): the JS client
gets no objects at all. Capture what imquic sends (stream types, subgroup
headers) and compare against what js/net accepts on draft-22 before deciding
which side is wrong.

Public API: none expected. Wire: none on our side unless js/net misreads draft-22.

## Related

- [Interop coverage](/quest/m1/interop-coverage/README.md) - the cases and clients
