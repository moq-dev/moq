# [M] PROXY through hops without it

## Goal

A proxied stream crosses a hop whose version lacks PROXY: any moq-transport
hop, and a moq-lite hop on a version before PROXY's, as between two relay
releases mid-rollout. moq-net's session on that hop carries it as two tracks under a hidden `.proxy/` namespace, one group each
way, and the next moq-lite hop turns them back into a PROXY stream. A dialer
and a listener see the same API whether the path has a moq-transport hop or
not. An interop test runs a stream through a moq-transport relay between two
moq-lite hops, and a sim test through a relay pinned to a moq-lite version
before PROXY's.

## Plan

See the [epic](/quest/m3/proxy/README.md) for the decisions.

- The encoding is Kixel's two-track byte stream, moved here from
  [kixel-ai/kixel.ai's socket/stream.rs](https://github.com/kixel-ai/kixel.ai/blob/main/rs/kixel/src/socket/stream.rs): one group per
  direction, a finished group as FIN, and a reset group with an application
  code as RST. Carry over what Kixel has landed on it by then: resuming a
  session within its track's max-age (finished in kixel.ai#148) and a
  buffer-sized cache per hop (now kixel.ai's `quest/m1/proxy.md`).
- The `.proxy/` prefix's leading `.` hides it from announce discovery.
- Admission stays the [grants](/quest/m3/proxy/grants.md): a converting hop
  admits the `.proxy/` tracks of a dial only under `dial` on the broadcast,
  and serves a listener's only under `listen`. Publish and subscribe grants on
  `.proxy/` paths admit nothing, so the track form never widens a session.

Public API: none; the stream's API is unchanged. Wire: none new, since the
track form is ordinary tracks.

## Required

- [PROXY over moq-lite](/quest/m3/proxy/stream.md) - the stream and API this carries across moq-transport
