# [L] Stateless broadcasts

## Goal

moq-net and `@moq/net` broadcasts have no finished state. `Producer::finish`,
`Finish`, `Finish::linger`, `Finish::wait`, and the origin's finished-only
supersession (`Consumer::supersede`, `routes.superseded`, `Announcer::superseded`,
`BroadcastState.finished`/`superseded`) are deleted, along with moq-ffi's
`MoqBroadcastFinish` and moq-c's `moq_publish_finish` and
`moq_publish_finish_close`, and their wrappers in every binding. A publisher
ends its content by finishing its tracks (hang: the catalog) and goes offline
by closing the broadcast.

## Plan

Decided with the maintainer on 2026-10-23; see the epic for why.

- Keep the epoch routing that lets a restarted publisher win
  (#6452); only supersession of a finished broadcast goes.
- Prove the property this relies on before deleting: a relay keeps
  delivering a finished track to its existing downstream subscribers after
  the upstream publisher's session closes, on lite and moq-transport. Fix
  it at the source if it does not.
- The interop `finish` case must keep passing on lite. Each interop
  publisher (Rust through the moq-mux helper; JS, C, C++ and the bindings by
  holding their finished broadcast open) keeps its broadcast up long enough
  for the late readers.
- `js/publish` drops its `Finish` bookkeeping
  (`js/publish/src/broadcast.ts`, the catalog's `Number.MAX_SAFE_INTEGER`
  max-age only if nothing else needs it). `js/net` drops `hooks.linger` and
  its `CACHE_WINDOW` use there.
- Update `doc/concept` with one paragraph on the model (online = announced,
  end of content = track FIN, retention = app policy), plus `doc/lib/*`, and
  the Impact bullet says how to migrate every affected package. It
  supersedes the `MIGRATE.md` entries on `finish` (#5795, #6157, and the
  binding entries measured against release's `finish()` alias for
  `close()`), for the cut to collapse.
- Coordinate with [JS origin publications](/quest/m1/api-audit/js-publication.md)
  and [binding publications](/quest/m1/api-audit/binding-publication.md),
  which touch the same handles.

Public API: breaking in moq-net, `@moq/net`, moq-ffi, moq-c, and every
binding. Wire: none.
