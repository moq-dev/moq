# [M] Document path patterns and wildcard advertisements

## Goal

A reader of doc.moq.dev can learn, without opening a PR or a draft, what a
path pattern is, how to advertise one, what a subscriber sees, and what each
binding calls the three operations. Every surface the dev PRs touched has a
page that says so before the release ships them.

## Plan

What landed on `dev` and where it is documented today: `moq-pattern` and
`@moq/pattern` (#3631), patterns in `moq_net::path` and `@moq/net` (#3458),
`create_broadcast`, `announce`, and `dynamic` aligned across the bindings
(#3577), Pattern events on advertised routes (#3649), and the drafts (#3457).
Only `doc/concept/moq-lite.md` mentions any of it, in one "Path patterns"
section; the library pages, the relay pages, and the binding docs do not.

- `doc/concept/moq-lite.md`: the section grows the advertisement half: what
  `dynamic(pattern, route)` claims, that a wildcard is a capability rather
  than an inventory, what a subscriber sees rebased under its scope, and how
  refusal answers a path the advertiser will not serve. Link the drafts for
  the wire.
- `doc/lib/rs/moq-net.md` and `doc/lib/js/net.md`: `Pattern` and `Patterns`
  with `contains`, `overlaps`, `rooted`, `rebase`; the announce events that
  now carry a pattern; and the three operations as #3577 aligned them:
  `origin.create_broadcast(path)` returning a producer whose
  `announce(route)` and `unannounce()` own the advertisement, against
  `origin.dynamic(pattern, route)` for a claim over a pattern. Take the
  names from the code at the time of writing, not from this quest, and cover
  the FFI-generated bindings in their own pages where one exists.
- `doc/bin/relay/cluster.md`: wildcard advertisements are forwarded and
  costed like any route; state the containment rule against the publisher's
  grant.
- Token scope stays prefix-based until path-patterns lands its claims; say
  that in one line where a reader would otherwise expect pattern grants.

Verify the way the docs are verified: every snippet compiles or runs under
the doc build, and `just check` on `doc/` is green.

## Related

- [Wildcard](/quest/m2/wildcard/README.md) - Resolve and Demand follow the
  release; their docs follow them
- [Path patterns](/quest/m2/path-patterns/README.md) - the grammar and the
  matcher the pages describe
