# [S] Keep-alive derives from the idle timeout

## Goal

The QUIC keep-alive interval defaults to a value derived from the idle
timeout, not a fixed 3 s, so changing `quic.idle_timeout` never needs a
matching hand-tuned `quic.keep_alive`. An explicit `quic.keep_alive`
(`moq_tokio::quic::Config::keep_alive`) stays an override, for a NAT binding
that lives shorter than the idle timeout, and `0s` still disables it.

## Plan

Decided 2026-10-08: shrunk to deriving the default. Replacing the keep-alive
timer with a PING fired near the idle deadline was not worth a config and
CLI break.

Today `moq-quic` arms `Timer::KeepAlive` a fixed interval after each packet
(`reset_keep_alive`), and moq-tokio's `DEFAULT_KEEP_ALIVE` is a hand-picked
fraction of `DEFAULT_IDLE_TIMEOUT`. Derive the interval from the negotiated
idle timeout instead (the smaller of ours and the peer's `max_idle_timeout`,
which the connection already holds), keeping it under a third so a lost PING
and its probe land before the peer's timer. Pick how the config expresses
"derived" (an `Option`, or a sentinel) by what reads clearest in the
`[quic]` section and CLI; document it in `doc/bin/relay/config.md`.

iroh stays on upstream noq. [Listener deadlines](/quest/m1/listener-deadlines.md)
wires `quic.keep_alive` into iroh's `keep_alive_interval`; with no explicit
value, iroh gets the same derivation from its idle timeout. The qmux
WebSocket keep-alive (`qmux::ws::KeepAlive`, a fixed 5 s ping and 30 s
deadline) is a different mechanism and stays as is.

Tests: the derived interval follows a changed idle timeout, including a peer
that advertises a shorter one; an idle connection survives with the
default; an explicit value wins; `0s` disables.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the derivation lands in `moq-quic`, not the frozen fork

## Related

- [Listener deadlines](/quest/m1/listener-deadlines.md) - wires the same setting into iroh's fixed interval
- [noq#810](https://github.com/n0-computer/noq/issues/810) - the deadline-driven proposal to n0; flub and matheus23 asked to keep a cap for NAT bindings, which the override covers
