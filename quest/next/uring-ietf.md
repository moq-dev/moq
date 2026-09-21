# [M] The io_uring workers serve moq-transport sessions

## Goal

A relay started with `--runtime-io-uring` serves every version its
configuration lists, IETF moq-transport included, instead of filtering to
moq-lite and warning that the rest are not served. A fleet that moves to the
ring then drops no client protocol over raw QUIC.

## Plan

`rs/moq-relay/src/uring.rs` states "Sessions are moq-lite only": `bind`
keeps `versions.iter().filter(|v| v.is_lite())`, refuses the lite versions
that negotiate in SETUP, and warns about configured moq-transport versions.
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
ring requires the release carrying it.

## Related

- [Stream sessions](/quest/next/uring-tcp/README.md) - the other protocol gap
  on the ring, WebSocket and HTTP
