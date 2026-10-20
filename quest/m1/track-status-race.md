# [XS] Report relays that refuse TRACK_STATUS

## Goal

Each relay found refusing TRACK_STATUS, or answering it with an undefined
code, has an upstream report. moqx is known: it answered REQUEST_ERROR
`0xFFFFFFFF` on draft 18 ([#5954](https://github.com/moq-dev/moq/issues/5954)).
Delete this quest once every known relay has one.

## Plan

The fix landed in [#5991](https://github.com/moq-dev/moq/pull/5991): a
subscribing reader registers its subscription before its request is visible,
so it reaches the publisher as a SUBSCRIBE, and the NOT_SUPPORTED fallback is
gone. Fetch-only and query-only demand still uses TRACK_STATUS and fails
loudly when refused, so a refusing relay now breaks only those. The
maintainer asked for the upstream reports on 2026-10-19; #5991 deferred them.

Posting outside the `kixelated` and `moq-dev` orgs needs the maintainer's
approval, so an agent drafts the report (the observed code, the draft, and
the TRACK_STATUS request that drew it) and the maintainer files or approves
it.
