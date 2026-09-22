# [S] The fetch verb prints one group

## Goal

`moq --connect <url> --broadcast <name> fetch <track> [--group N]` writes one
group of a track to stdout, the MoQ equivalent of the relay's HTTP
`/fetch/<broadcast>/<track>?group=N`. With no `--group` it fetches the latest
group, as the endpoint does. By default stdout carries the frame payloads
concatenated, byte-identical to `curl /fetch`, so a JSON catalog pipes into
`jq`. `--json` instead prints one line per frame with its group sequence,
frame index, size, and base64 payload.

## Plan

- A MoQ verb like `ls`: a subscriber-only session that never publishes. It
  exits zero after the group's last frame and non-zero on not found, refusal,
  or timeout, with the reason on stderr.
- Reuse the endpoint's semantics rather than copy its code: "latest" needs a
  subscription to learn the newest sequence, then fetches it so an evicted
  group is retrieved from upstream (`serve_fetch` in
  `rs/moq-relay/src/web.rs`). If both need the helper, move it into a library
  both depend on rather than keeping two copies.
- Update `doc/bin/cli.md` beside `ls`.
- Test: against an in-process relay, fetching a known sequence prints its
  exact bytes, the default fetches the newest group, a missing sequence exits
  non-zero, and `--json` lines parse and decode to the same bytes.
