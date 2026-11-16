# [M] The gateways admit a session the same way

## Goal

moq-webrtc, moq-rtmp, moq-srt, and moq-rtsp read as one family. An ingest's
rate cap is a field of `moq_mux::Ingest`, so WHIP, SRT, RTMP, and RTSP
ingest are capped by one setting. Every accept, pull, and publish in
moq-rtmp and moq-srt takes a `#[non_exhaustive]` request struct, as
moq-webrtc's do, so a later option is a new field. The WHEP pull is
`moq_webrtc::Client::pull`, like the other three. The CLI endpoint is
`moq import webrtc` and `moq export webrtc`; `rtc` is refused at startup
with a message naming the new spelling.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Align the gateways before the cut: the cap on the shared ingest type,
  `subscribe` renamed `pull`, and request structs in moq-rtmp and moq-srt
  (rejected: only the two renames and the cap's home; keep and take the
  breaks in m3). moq-rtmp and moq-srt already break in this cut and
  moq-webrtc has never been published, so the cut pays for it either way.
- ✅ The CLI endpoint becomes `webrtc`, with `rtc` refused by name
  (rejected: keep `rtc`; name the endpoints `whip` and `whep`, since each is
  both directions under `import` and `export`). The crate is `moq-webrtc`
  and the auth transport an operator sees is `"webrtc"`
  (`rs/moq-auth/src/request.rs`).

Facts found by the audit:

- Four admission shapes. WebRTC: a free function and a `#[non_exhaustive]`
  struct, `whip::accept(&server, whip::Request { .. })`
  (`rs/moq-webrtc/src/server/whip.rs:98`, `:160`) and `whep::accept`
  (`rs/moq-webrtc/src/server/whep.rs:111`, `:155`). RTMP: positional
  arguments and setters, `Publish::accept(self, origin, path, &ingest)`
  (`rs/moq-rtmp/src/server.rs:601`) and `Play::with_delay(..).accept(origin,
  path)` (`:750`, `:762`); its client is `publish(self, stream_key, source)`
  and `pull(self, stream_key, origin, path, ingest)`
  (`rs/moq-rtmp/src/dial.rs:294`, `:382`). SRT: the same on the server
  (`rs/moq-srt/src/server.rs:360`, `:379`, `:432`, `:440`, `:451`) and a
  half-converted client, where `publish(&self, origin, dial::Publish)` takes
  a struct and `pull(&self, origin, path, ingest)` does not
  (`rs/moq-srt/src/dial.rs:106`, `:122`, `:151`). RTSP:
  `Client::pull(&self, ..)` (`rs/moq-rtsp/src/client.rs:121`).
- Names: the WHEP pull is `moq_webrtc::Client::subscribe`
  (`rs/moq-webrtc/src/client/mod.rs:65`); the other three say `pull`.
  RTMP's server egress request is `Play`, SRT's is `Subscribe`, and
  `moq_srt::dial::Publish` sits beside the unrelated server-side
  `moq_srt::Publish`.
- Two recorded decisions (2026-11-07, in
  [the SRT rate handle](/quest/m3/srt-rate-handle.md) and
  [WHEP pacing](/quest/m3/rtc-pacing.md)) say a rate handle reaches a
  session as a struct field, never a `with_*` setter. SRT egress has only
  setters, so its m3 cap would break the signature or the decision.
- The ingest cap is split: WHIP carries `whip::Request.rate`
  (`rs/moq-webrtc/src/server/whip.rs:108`), while the SRT quest proposes "a
  field on `moq_mux::Ingest`" (`rs/moq-mux/src/ingest.rs:12`, which already
  holds `max_age`, `bandwidth`, and `linger`).
- The CLI spells the endpoint `rtc`: `ImportSource::Rtc` and
  `ExportSink::Rtc` (`rs/moq-cli/src/args.rs:861`, `:1095`, `"rtc"` at
  `:1113`), guide at `doc/bin/rtc.md`. The crate rename
  ([moq-dev/moq#8122](https://github.com/moq-dev/moq/pull/8122)) did not
  consider the endpoint.

For the PR, in this order:

1. `rate: Option<moq_net::bandwidth::Limit>` on `moq_mux::Ingest`; delete
   `whip::Request.rate` and read the ingest's. WHIP keeps enforcing it
   (TIAS, REMB, and the meter); SRT, RTMP, and RTSP ingest carry the field
   and enforce it when their own quests land, and say so in the field's
   docs, so an embedder is not told a cap holds where it does not yet.
   Update [the SRT rate handle](/quest/m3/srt-rate-handle.md)'s API line.
2. Request structs for moq-rtmp (`Publish`, `Play`, and the client's
   `publish` and `pull`) and moq-srt (`Publish`, `Subscribe`, and the
   client's `pull`), replacing positional arguments and `with_*` setters.
   Settle the two name clashes while the types move: one word for the
   server egress request across RTMP and SRT, and `dial::Publish` against
   the server's `Publish`.
3. `moq_webrtc::Client::subscribe` becomes `pull`.
4. `import webrtc` and `export webrtc`, with `rtc` kept as a hidden legacy
   spelling refused at startup, the way renamed flags already are
   ([moq-dev/moq#8747](https://github.com/moq-dev/moq/pull/8747));
   `doc/bin/rtc.md` moves to `doc/bin/webrtc.md` with its inbound links, and
   `rs/moq-cli/src/rtc.rs` follows.

Each crate's `MIGRATE.md` gets its note; `rs/moq-webrtc/MIGRATE.md` is still
titled "Migrating moq-rtc". No behaviour changes, so the existing gateway
tests are the proof, plus one CLI test that `import rtc` is refused with the
new name in the message.

Public API: moq-rtmp, moq-srt, and moq-mux break; moq-webrtc changes while
unpublished. CLI: the `rtc` endpoint is renamed. Wire: none.

## Related

- [TS import reports its outcomes as values](/quest/m2/ts-typed-outcomes.md) - removes the CLI's downcasts of moq-srt and moq-rtsp errors
- [moq-srt per-connection rate handle](/quest/m3/srt-rate-handle.md) - enforces the ingest cap this gives a home, and adds the egress cap as a request field
- [moq-webrtc paces WHEP viewers](/quest/m3/rtc-pacing.md) - adds `rate` to `whep::Request`
