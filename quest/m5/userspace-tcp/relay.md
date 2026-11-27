# [M] The relay serves a TCP listener from user-space TCP

## Goal

An operator sets one relay config option naming an interface, and the relay's TCP
listener port is served from the user-space stack. QMux over `tcp://`,
`tls://`, `ws://`, and `wss://` gets late commit and moq-congestion. HTTP (axum
and hyper) and everything else on the port runs over its stream. Startup fails
with a clear error when XDP cannot attach. Kernel TCP stays the default.

## Plan

- The congestion controller comes from the same selector the QUIC listener uses
  (`moq_tokio::quic::CongestionControl`), so one setting covers both.
- Document the option in `doc/bin/relay/`, with the capabilities it needs and
  the XDP mode it reports.
- Public API: a new relay config option, which is additive. Wire: none.

## Required

- [Comparison](/quest/m5/userspace-tcp/measure.md) - the go/no-go for shipping it

## Hosts

- linux - AF_XDP is Linux only

## Related

- [One port](/quest/m3/one-port/README.md) - the TCP port this stack would own
- [TCP streams on the worker](/quest/m4/stream-transports/ring.md) - the later home of the XSK rings
