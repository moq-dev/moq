# [L] PROXY over moq-lite

## Goal

moq-lite gains a `PROXY <broadcast>` stream: one bidirectional stream of
opaque bytes. A relay forwards it toward the broadcast's publisher along the
route a subscription would take, holding about a stream window per hop and
caching nothing. moq-net offers it in Rust and JS with mirrored names: a
dialer opens a stream to a broadcast, and a publisher listens for streams on
its broadcast and accepts or refuses each. Finishing a side is a FIN, a reset
carries an application code, and a hop failure resets both sides. A sim test
carries a bulk transfer through two relays to a slow reader with each hop's
memory bounded by its window, and a publisher that does not listen refuses.

## Plan

See the [epic](/quest/m3/proxy/README.md) for the decisions.

- Admission is the [dial and listen grants](/quest/m3/proxy/grants.md).
- A new moq-lite stream type is a wire change: follow AGENTS.md's wire rules
  for the version it joins and update its draft in the same PR.
- The relay forwards bytes, not frames: flow control on each hop's stream is
  the only buffer.
- Kixel's byte stream semantics are the reference: FIN as finish, RST with a
  code, as in kixel-ai/kixel.ai's `rs/kixel/src/socket/stream.rs`.

## Required

- [Dial and listen grants](/quest/m3/proxy/grants.md) - the permissions a relay checks before forwarding a PROXY
