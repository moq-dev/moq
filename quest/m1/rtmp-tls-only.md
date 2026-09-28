# [XS] RTMP listener can refuse plaintext

## Goal

An operator who configures TLS on the RTMP ingest listener can require it.
Today a TLS-configured listener sniffs the first byte and serves any
non-ClientHello connection as plaintext `rtmp://` on the same port
(`rs/moq-rtmp/src/listen.rs`), so setting `tls` silently keeps accepting
unencrypted stream keys. The sniffing behavior stays available; it is just no
longer the only choice.

## Plan

Decided by the maintainer during the merged-PR audit: add a mode rather than
drop the sniffing. #3964 shipped mixed mode after a bot review, not a human
one, and nobody chose it for operators who want TLS only.

- Model it so a TLS-only listener without a TLS config cannot be expressed,
  for example an enum carrying the `ServerConfig` (plaintext, TLS required,
  TLS or plaintext) instead of a bool beside the `Option`. Keep today's
  behavior as the default for an existing `tls` config unless that reads
  wrong once written; say which in the PR.
- A plaintext connection to a TLS-only listener is refused at the first byte,
  with a log line naming the peer, not left to time out.
- Thread the choice through the relay and gateway configs that build this
  listener, and update `doc/bin/rtmp.md` and any relay config docs that
  describe RTMPS.
- Test both modes: a plaintext client is refused by TLS-only and served by
  mixed.

Public API: additive on moq-rtmp's listen config if the default holds; a
published break (dev) if the field's type changes. Wire: none.
