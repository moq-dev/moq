# [XS] Smoke clients use request and follow

## Goal

Condition: a binding release ships the `request` and `follow` names from
[moq-dev/moq#7193](https://github.com/moq-dev/moq/pull/7193). Check each
registry's latest moq-ffi-based package (Go, Python, Kotlin, Swift, C) for
`request`/`follow`. Then move `test/smoke/clients/{go,python,kotlin,swift,c}`
off `request_broadcast` and `announced_broadcast`, and delete this quest.

## Plan

Decided 2026-10-29 (maintainer). The smoke clients install published packages
(see `test/smoke/AGENTS.md`), so they cannot move until the rename ships.
[moq-dev/moq#7173](https://github.com/moq-dev/moq/pull/7173) handled the JS
smoke client the same way. If a client must run against both old and new
releases for a while, detect the method, as the JS compat client does.

Public API: none. Wire: none.
