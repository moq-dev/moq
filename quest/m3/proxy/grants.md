# [M] Dial and listen grants

## Goal

`moq_auth::Grant` and its JS counterpart carry two more `Access` fields,
`dial` and `listen`, read from tokens and auth endpoint answers alike. A
relay admits `PROXY <broadcast>` only from a session whose `dial` covers the
broadcast, and delivers one only to a publisher whose `listen` covers it.
Neither `publish` nor `subscribe` implies either.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan), with the
[epic](/quest/m3/proxy/README.md): explicit grants, so a token that only reads
a media broadcast can never open a stream to its publisher.

- An unknown field is already refused (`Grant`'s docs), so a relay that
  predates these fields refuses a grant carrying them rather than widening it.
  Note it in the Impact section with the token and auth endpoint formats that
  change.
- lite-07's `AUTH_OK` carries a session's publish and subscribe grants, so it
  carries `dial` and `listen` too, a wire change in the current wip version.
- Grants become path prefixes in
  [moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251)
  (`quest/m2/prefix-grants.md`); `dial` and `listen` are prefix sets like the
  others, so build on it once it lands.
- A cluster link forwards streams for many sessions, so a link's grant covers
  the `dial` and `listen` it relays, as its publish and subscribe already do.
- Mirror the names in Rust, JS, and the docs. moq.pro's dashboard tokens and
  auth endpoint answers, and kixel.ai's `/mesh/auth`, adopt the fields on
  their side.

Public API: additive, `Grant::dial` and `Grant::listen` and their JS
counterparts. Wire: the token and auth endpoint formats gain two fields that
an older reader refuses, and lite-07-wip's `AUTH_OK` gains them.
