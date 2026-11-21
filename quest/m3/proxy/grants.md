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
- Mirror the names in Rust, JS, and the docs. moq.pro's dashboard tokens and
  auth endpoint answers, and kixel.ai's `/mesh/auth`, adopt the fields on
  their side.
