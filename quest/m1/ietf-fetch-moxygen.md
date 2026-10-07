# [M] Draft-20 FETCH interoperates with moxygen

## Goal

Interop covers IETF FETCH on draft 20 and later against moxygen in both
directions: moxygen fetching from our relay or publisher, and our relay
fetching from moxygen. Each failure found is fixed at its cause or filed as
its own quest.

## Plan

Found by #4971 (draft-20 FETCH): nothing tested here exercises IETF FETCH
against another implementation, and the in-tree interop matrix has no
external peers.

Decided 2026-10-07: run moxygen locally against this checkout, with no change
to moq-interop-runner. A just recipe or a cell in the in-tree interop harness
(`test/interop`) starts a moxygen relay or client next to ours; pick whichever
fits how moxygen is built or pulled (a pinned container image avoids building
it from source). Cover a fetch within one group, a joining fetch, and a
refused multi-group range. Wire it into CI, at least nightly.

Public API: none. Wire: none expected.

## Related

- [Runner approval](/quest/m3/interop-runner-approval.md) - not needed here; only a later move of these cases into moq-interop-runner would wait on it
