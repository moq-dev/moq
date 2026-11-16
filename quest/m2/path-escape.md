# [L] An escaped slash never splits a path segment

## Goal

A path segment is the text between two unescaped `/`: `a\/b` is one segment
that holds a `/`, everywhere. The 32-segment limit, prefix matching, `head`,
hidden detection, announce compression, auth patterns, discovery, stats
buckets, and `@moq/net` all read segments through one escape-aware walk, so
a moq-transport namespace tuple maps to the same segments on both wires, and
the lite draft says what a segment is. Today the escape is only validated:
every one of those places still splits at every `/`.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Finish the escape (rejected: lite paths have no escape and the
  moq-transport layer owns the mapping; keep the escape and only write the
  split-everywhere reading into the draft). The maintainer's reason:
  moq-transport namespaces are tuples, and `\` is the delimiter rule we
  invented to carry one in a path, so it has to be complete before it
  freezes.

Facts found by the audit:

- `Path::new` checks escapes (`validate_escapes`,
  `rs/moq-net/src/path/mod.rs:87`) and its own doc says "Segments still
  split at every `/`". `Path::parts` is `split_terminator('/')`, and the
  segment limit (`MAX_PARTS`, `:663`, `:672`) and `is_hidden` (`:299`) are
  built on it. `Path::head(n)` returns `None` when the cut would land on an
  escaped `/` (`:259`), so such a prefix can't be taken at all.
- The moq-transport mapping already walks real segments:
  `escaped_fields` splits "at every `/` no escape covers"
  (`rs/moq-net/src/ietf/namespace.rs:7`). So a namespace field holding `/`
  is one tuple field on moq-transport and two segments on moq-lite: it
  counts twice against the limit, a pattern's `*` matches half of it, and
  `a\/.b` is hidden though no field starts with `.`.
- The draft defines the escape (Broadcast section: "A `\` (0x5C) escapes
  the `\` or `/` that follows it") and matches "per path segment", without
  saying which reading of a segment applies.
- Splitters outside moq-net, each `split('/')` on a path:
  `rs/moq-auth/src/path.rs:39`, `rs/moq-auth/src/tier.rs:14`,
  `rs/moq-pattern/src/pattern.rs:795,814`, `rs/moq-relay/src/auth.rs:1017`,
  `rs/moq-stats/src/produce.rs:1338`, `rs/moq-cli/src/report.rs:492`,
  `rs/moq-cli/src/archive.rs:69,274`, `rs/moq-hls/src/export/route.rs:96`.
  JS: `js/net/src/path.ts:99,254,286,289,314,317,366,369`,
  `js/net/src/origin.ts:728,775,791`, `js/stats/src/track.ts:40`. Sort
  each into "a broadcast path" (change it) or "a URL or file path" (leave
  it); `rs/moq-relay/src/web.rs:1059` and `cluster/key.rs:46` look like
  URLs.

Guidance for the PR:

- One walk owns the rule: `Path::parts` yields segments split only at an
  unescaped `/`, still escaped as written, and `escaped_fields` becomes a
  caller of it. `head`, `has_prefix`, `strip_prefix`, `join`, `Relative`'s
  resolution, the limit, and `is_hidden` follow, and `head` stops returning
  `None` for an escape. Give other crates the walk and, where one compares
  a segment's content, its unescaped bytes; no crate outside moq-net
  splits a broadcast path by hand afterwards.
- `parts` is on the route table's lookup path, so keep a path with no `\`
  on the plain split and show `rs/moq-net/benches/path.rs` and
  `benches/routes.rs` unchanged for it.
- Hidden detection reads the first byte of the unescaped segment. Patterns
  in `moq-pattern` and auth grants match whole segments; decide in the PR
  whether a pattern itself may contain an escape, and recommend yes, with
  the same grammar, so a grant can name a field holding `/`.
- A path that maps to a file or URL (archive directories, HLS routes,
  stats bucket names) needs an encoding for a segment holding `/` or `\`
  that can't escape its directory; refuse such a segment there unless one
  already exists.
- Mirror it in `js/net/src/path.ts` first, since `origin.ts` and
  `@moq/stats` build on it. The bindings take paths as strings and need no
  API change; add one escaped path to their interop cases.
- Draft: one sentence in Broadcast defining a segment as above, and that a
  prefix never ends inside an escape. `doc/concept` wherever paths are
  described.
- Tests: a moq-transport publisher whose namespace field holds `/` crosses
  a relay to a moq-lite reader and back with the same tuple; a grant
  pattern matches that field as one segment; a 32-field namespace with
  escaped separators is accepted; the fuzz target for paths
  (`rs/moq-net/src/fuzz.rs`) covers the walk. Run `just test interop
  --all`.

Public API: `Path::parts`, `head`, and the prefix operations change what
they return for a path with an escaped `/`, in Rust and `@moq/net`; no
signature changes. Wire: lite-07-wip, for paths that contain an escape
only (segment count, prefix and pattern matching, hidden detection).

## Related

- [Finalize lite-07](/quest/m2/lite07-finalize.md) - freezes the path rules this completes
