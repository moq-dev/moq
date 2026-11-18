# [M] Announce listings report present or absent

## Goal

An announce listing reports two kinds in every language: a path became
present, or it became absent. `moq_net::announce::Event`, js/net
`Announced.next`, moq-ffi `MoqAnnounceEvent`, and libmoq `moq_announce_kind`
all shrink to those two. A let-go (`Offline`) is reported as absent, and a
route's availability is read from `announce.route.available()` or a
`follow()`, not from the listing.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan): ✅ Present / Absent only
(rejected: Present / Absent / Changed, keeping one availability event; keeping
all five kinds with an added `present()` helper).
[moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375) added
`Unavailable`, `Available`, and `Offline` on purpose, but no consumer acts on
them: moq-tokio's origin and server, moq-cli's `announced`, `report`, and
`archive`, moq-boy, moq-webrtc, js `room`, and js `watch` each fold
`Absent | Offline` into "gone" and skip the availability kinds. Only examples
and libmoq/moq-ffi forwarding read all five.

Find the consumers with `rg 'announce::Event|AnnounceEvent|MoqAnnounceEvent|moq_announce_kind'`
and the JS `kind` checks on `Announced.next`. Recommended: in Rust keep a
two-variant enum carrying `Announce`; in moq-ffi, a record
`{ announce, present }` avoids an exhaustive switch in every binding. Check
that nothing on the relay side (origin internals, the lite announce stream)
reads `AnnounceEvent` rather than `RouteEvent`; the wire's let-go handling
from #8375 is not this listing and stays.

Public API: breaking in moq-net, js/net, moq-ffi and its bindings, and
libmoq; migrate `Absent | Offline` to absent and drop availability arms.
Wire: none.
