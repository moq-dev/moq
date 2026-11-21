# [M] PROXY through moq-transport

## Goal

A proxied stream crosses a moq-transport hop. moq-net's moq-transport session
carries it as two tracks under a hidden `.proxy/` namespace, one group each
way, and the next moq-lite hop turns them back into a PROXY stream. A dialer
and a listener see the same API whether the path has a moq-transport hop or
not. An interop test runs a stream through a moq-transport relay between two
moq-lite hops.

## Plan

See the [epic](/quest/m3/proxy/README.md) for the decisions.

- The encoding is Kixel's two-track byte stream, moved here from
  kixel-ai/kixel.ai's `rs/kixel/src/socket/stream.rs`: one group per
  direction, a finished group as FIN, and a reset group with an application
  code as RST. Carry over what Kixel has landed on it by then: resuming a
  session within its track's max-age (its `quest/m0/mesh/reaccept.md`) and a
  buffer-sized cache per hop (its `quest/m1/stream-memory.md`).
- The `.proxy/` prefix's leading `.` hides it from announce discovery.

## Required

- [PROXY over moq-lite](/quest/m3/proxy/stream.md) - the stream and API this carries across moq-transport
