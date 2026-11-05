# [XS] Smoke clients use request and follow

## Goal

`test/smoke/clients/{go,python,kotlin,swift}` call `request` and `follow`
instead of `request_broadcast` and `announced_broadcast`, and handle the
optional result a followed broadcast now gives once it goes offline, instead
of `announcedBroadcast(...).available()`.

## Waiting

- moq-dev/moq: a binding release ships the `request` and `follow` names from
  [moq-dev/moq#7193](https://github.com/moq-dev/moq/pull/7193) and the path
  listing and offline-reporting `follow` from
  [moq-dev/moq#7678](https://github.com/moq-dev/moq/pull/7678). Check each
  registry's latest moq-ffi-based package (Go, Python, Kotlin, Swift) for
  `request`/`follow` and #7678's optional result.

## Plan

Decided 2026-10-29 (maintainer). The smoke clients install published packages
(see `test/smoke/AGENTS.md`), so they cannot move until the rename ships.
[moq-dev/moq#7173](https://github.com/moq-dev/moq/pull/7173) handled the JS
smoke client the same way. If a client must run against both old and new
releases for a while, detect the method, as the JS compat client does.

Decided 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run): #7678's optional result joins this quest
rather than a separate one, since both changes reach the same clients through
the same release. Today's call sites: `test/smoke/clients/swift/Sources/smoke/main.swift`,
`test/smoke/clients/kotlin/src/main/kotlin/Main.kt`, and
`test/smoke/clients/python/smoke.py`.

The C smoke client is out of scope: it uses the hand-written `moq-c`, which
keeps `moq_origin_announced_broadcast`, and
[Published generated C](/quest/m2/release/c-publish.md) moves it onto the
generated API, which already names them `request` and `follow`.

Public API: none. Wire: none.
