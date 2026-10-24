# [XS] The Go reservation test waits for a grant

## Goal

`TestReservationUpdateReleasesSubscribedTrack` (`go/wrapper/bandwidth_test.go`)
never fails with "grant unavailable".

## Plan

`Reservation.Grant()` returns nothing until the QUIC estimate exists, and the
test asserts right after `Dial`. Seen under load in
[moq-dev/moq#6476](https://github.com/moq-dev/moq/pull/6476) and
[moq-dev/moq#6517](https://github.com/moq-dev/moq/pull/6517). Wait on the
grant's own signal rather than a sleep.

Public API: none. Wire: none.
