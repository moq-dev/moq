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
(`reset_keep_alive`, reading the shared `TransportConfig`), and moq-tokio's
`DEFAULT_KEEP_ALIVE` is a hand-picked fraction of `DEFAULT_IDLE_TIMEOUT`.
The timeout that matters is the negotiated one (RFC 9000 section 10.1): the
smaller of ours and the peer's `max_idle_timeout`, which `moq-quic` sets when
the peer's transport parameters arrive. A peer that advertises a shorter
`max_idle_timeout` than our config (2 s against our 10 s) times out a quiet
connection under our fixed 3 s keep-alive, so the keep-alive has to track the
negotiated value. Fixing that per connection from outside `moq-quic` means
building a `TransportConfig` per connection.

Derive the interval inside `moq-quic`, where the negotiated timeout is set,
keeping it under a third so a lost PING and its probe land before the peer's
timer. The derived interval lives in connection-local state on `Connection`,
which `reset_keep_alive` reads; the shared `Arc<TransportConfig>` keeps only
the configured value. moq-tokio's and moq-uring's drivers, and anything else
on `moq-quic`, then get it with no plumbing. An explicit `quic.keep_alive`
longer than the negotiated idle timeout is capped at the derived interval,
since a keep-alive longer than the idle timeout can never keep the connection
up. Pick how `TransportConfig` and the config express "derived" (an `Option`,
or a sentinel) by what reads clearest in the `[quic]` section and CLI;
document it in `doc/bin/relay/config.md`.

Rejected: a public `Connection::idle_timeout()` and a live per-connection
`set_keep_alive_interval`. Once the derivation is inside `moq-quic`, neither
has a consumer, and each is another divergence from quinn for the fork to
carry.

The qmux WebSocket keep-alive (`qmux::ws::KeepAlive`, a fixed 5 s ping and
30 s deadline) is a different mechanism and stays as is.

The acceptance test: a connection whose peer advertises a shorter idle
timeout than ours stays up while idle, its PINGs spaced below the negotiated
timeout, with no per-connection config. Also test that the derived interval
follows a changed local idle timeout: with a 30 s peer timeout, a local 10 s
and then 5 s each become the negotiated timeout, and the interval shrinks
with it. An idle connection survives with the default; an explicit value
wins, except one longer than the negotiated timeout, which is capped at the
derived interval; `0s` disables.
