# [S] Timed moq-transport tracks cross a lite-07 mesh

## Goal

A timed track ingested over moq-transport keeps its timestamps across a
lite-07 hop to a peer that negotiated the `ietf` extension, so a relay mesh
carries them end to end. A peer without the extension still gets the track
untimed.

## Plan

Implementation is complete in
[moq-dev/moq#6300](https://github.com/moq-dev/moq/pull/6300).
The original moq-transport version is the sole unchecked-timestamp provenance;
the duplicate Rust and JS flags are removed. Rust serves that track timed in
TRACK_INFO, listings, SUBSCRIBE, FETCH, and datagrams only when the lite-07
session negotiated `ietf`. Unknown and native moq-lite provenance retain the
normal rewind checks, and a peer declining either offer receives it untimed.

JS mirrors the ingress-version provenance and keeps serving IETF tracks untimed
over its lite sessions. The epic deliberately excludes JS `ietf` passthrough;
this quest does not add that separate implementation.

The former timestamp-loss test fails before the source change and passes after.
Hop coverage checks the original units and opaque property, real FETCH before
any subscription, a rewound later group, datagrams, and both declined offers.
The complete Rust net suite passes 2926 tests (8 existing skips), and the
focused JS suites pass 327 tests. The matching base and extension drafts and
the existing timing explanation are synchronized.

Retirement of this completed file and its links is the remaining action. The
session's automatic approval review has blocked completed-quest removal under
the unattended no-abandonment instruction; retain the accurate quest pending
narrow retirement authorization rather than bypass that block.
