# [XS] moq-quic is listed on the public QUIC interop matrix

## Goal

moq-quic appears on <https://interop.seemann.io>: its image is published and
registered in quic-interop/quic-interop-runner, so other implementers test
against it on their own schedule.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): ✅ list after our own nightly run is mostly green (rejected: never
listing; listing from day one). The registration is a PR outside the
maintainer's orgs, so it needs their go-ahead, and the repository's AI policy
is read before drafting it.

- Publish the endpoint image `test/quic-interop` builds, from `main`, and add the entry to `implementations_quic.json`.

Public API: none. Wire: none.

## Waiting

- Maintainer: approve registering on the public matrix once the nightly matrix is mostly green. An agent cannot decide when our failures become public or post to quic-interop unasked. Recommendation: approve when every supported case passes against at least two peers.
