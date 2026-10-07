# [S] Draft-20 FETCH interoperates with moxygen

## Goal

Interop covers IETF FETCH on draft 20 and later against moxygen in both
directions: moxygen fetching from our relay or publisher, and our relay
fetching from moxygen. Each failure found is fixed at its cause or filed as
its own quest.

## Plan

Found by #4971 (draft-20 FETCH): nothing tested here exercises IETF FETCH
against another implementation, and the in-tree interop matrix has no
external peers.

Decided 2026-10-07: run it through the community interop runner that already
pairs moq with moxygen, adding FETCH cases if it has none. Cover a fetch
within one group, a joining fetch, and a refused multi-group range.

Public API: none. Wire: none expected.
