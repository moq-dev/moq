# [S] Keep-alive by deadline

## Goal

A connection sends a PING only when its idle deadline is near, never on a
fixed clock. An idle connection with a 10 s idle timeout costs one packet per
roughly 10 s minus a few PTOs; a busy connection costs none. The
`keep_alive` setting on `quic::Client` and `quic::Server` becomes an optional
maximum, default none. Redefining it is a config and CLI break, so this
targets `dev`.

## Plan

noq-proto arms `Timer::KeepAlive` at `now + keep_alive_interval` on every
authenticated packet (`connection/mod.rs`, `reset_keep_alive`), so the
interval is a second, unrelated clock the operator has to keep below the idle
timeout by hand. Replace it in the fork:

- The deadline is the negotiated idle timeout, the minimum of the local
  setting and the peer's `max_idle_timeout` transport parameter, measured
  from the last packet sent or received. Fire the PING at
  `deadline - 3 * pto()`, bounded below at one PTO, so a lost PING and its
  probe still land before the peer's timer.
- Keep `keep_alive_interval` as a maximum: `None` means "as late as
  possible"; a value means "no later than this", for a NAT binding with a
  shorter life than the idle timeout. That is the only knob.
- The multipath per-path keep-alive follows the same rule per path.

moq-tokio's `quic.keep_alive` (`rs/moq-tokio/src/quic.rs`) in the `[quic]`
sections, CLI flags, and env vars becomes that optional maximum, default none,
documented in `doc/bin/relay/config.md` for NAT-sensitive deployments. The qmux WebSocket keep-alive (`qmux::ws::KeepAlive`,
5 s ping and 30 s deadline) already has this shape; make its wording match.

Tests: an idle connection survives an idle timeout with exactly one PING per
period; a busy connection sends none; a lost PING is probed before the
deadline; the maximum knob shortens the period.
