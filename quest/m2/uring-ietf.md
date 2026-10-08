# [M] The io_uring workers serve moq-transport sessions

## Goal

A relay started with `--runtime-io-uring` serves every version its
configuration lists, IETF moq-transport included, instead of filtering to
moq-lite and warning that the rest are not served. A fleet that moves to the
ring then drops no client protocol over raw QUIC.

## Plan

The io_uring listener's bind in `rs/moq-relay/src/uring.rs` advertises only
the lite ALPNs (`is_lite`), refuses a lite version named in the config that
negotiates in SETUP, and only warns when the config lists moq-transport
versions.
The tokio path already drives both wires through one session type, so the
gap is in the worker's accept path, not in moq-net.

- Accept the moq-transport ALPNs on the worker and hand the connection to
  the same session constructor the tokio workers use for them; the `!Send`
  local task set is the constraint to satisfy, as it was for lite.
- Serve the SETUP-negotiated lite versions the same way, or record why they
  stay refused; the operator-facing rule must be "the ring serves what the
  config lists".
- `tests/runtime_uring.rs` gains an IETF session and a SETUP-negotiated
  session; both skip loudly below the kernel floor like the rest.
- `doc/bin/relay/config.md` and the `moq-uring` README drop the lite-only
  caveat.

Additive, so it lands on main. moq.pro's fleet deploy of the
ring requires the release carrying it. Decided 2026-10-08: moved to m2 with
that fleet deploy, which is m2 work.

## Related

- [Stream sessions](/quest/m3/uring-tcp/README.md) - the other protocol gap
  on the ring, WebSocket and HTTP
