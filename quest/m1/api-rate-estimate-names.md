# [S] Every rate estimate is named estimated_*_rate on every surface

## Goal

The congestion controller's send estimate and the PROBE receive estimate carry
the same name in every language: `moq_net::ConnectionStats::estimated_send_rate`
/ `estimated_recv_rate` and `@moq/net`'s `estimatedSendRate` / `estimatedRecvRate`
already do; the C ABI, its bindings, and moqsink's top-level properties catch up.
Nothing public exposes an averaged `send_rate` or `recv_rate`: a rate over a
window is the caller's `bytes_sent` delta, and the only rates we hand out are
the instantaneous estimates, named as such.

## Plan

Today's outliers, at main `ea47bc344` plus #3679:

- `rs/moq-ffi/src/session.rs:420` `MoqConnectionStats { send_rate_bps,
  recv_rate_bps }` and `rs/libmoq/src/api.rs:446` `send_rate_bps` /
  `send_rate_valid` / `recv_rate_bps` / `recv_rate_valid`; py, swift, kt, go,
  and dart inherit the FFI names, `cpp/obs` reads the libmoq ones, and
  `doc/lib/{c,py,swift,kt,go,dart}` document them.
- moqsink's `estimated-send-bitrate` / `estimated-recv-bitrate` properties
  (`rs/moq-gst/src/sink/imp.rs`, `doc/bin/gstreamer.md`), which #3679's
  `connection-stats` structure already spells `estimated-send-rate-bps` /
  `estimated-recv-rate-bps`.

Rename them to `estimated_send_rate_bps` / `estimated_recv_rate_bps` (C ABI,
with matching `_valid` flags), `estimated-send-rate` / `estimated-recv-rate`
(moqsink), and the per-language casing of the same words in each wrapper.
Keep the moqsink properties: unlike the structure they notify on change. The
`send_bandwidth` / `recv_bandwidth` consumers on `moq_net::Session` and
`moq_native::Reconnect` name the channel, not the number, and stay.

Public API: breaking on moq-ffi, libmoq, every binding, and moq-gst, so on
dev. Wire: none. Run `just test smoke-full` for the bindings and the obs test.

## Required

- PR #3679 has merged - the `connection-stats` structure spelling this quest
  renames against comes from it

## Related

- [Reconnect stats handle](/quest/m1/api-reconnect-stats-handle.md) - the
  moq-native handle these estimates are read through gets a real name
