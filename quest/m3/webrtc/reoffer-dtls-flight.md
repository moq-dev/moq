# [S] A re-offered session's first DTLS flight reaches it

## Goal

A viewer that re-offers from the address its abandoned session still holds completes DTLS without waiting out a DTLS retransmit (about 0.9 s), because its first flight reaches the new session.

## Plan

Found while fixing [a_reoffer_from_the_same_address_plays](https://github.com/moq-dev/moq/pull/9241), which takes ~1 s of its 1.1 s in this wait on an idle host. In `server/mux.rs`, a datagram that is not STUN goes to the address's holder alone (`Datagram::Other`). The re-offering peer starts DTLS the moment its ICE completes, and its ClientHello can reach the mux 40 us before the new session's ICE connects and `Route::confirm` moves the address, so the old session receives and drops it. Only dimpl's first retransmit recovers.

Measured with a debug print in `Registry::route` and `Route::confirm`: ClientHello routed to the old holder with `claims.contains_key(src)`, then `confirm` 43 us later.

Options: route `Datagram::Other` to the claimant as well as the holder, as `Datagram::Answer` already does (check that str0m drops DTLS from a pair ICE has not verified, so a forged source gains nothing), or hold the claimant's own first flight until it confirms. Prefer the first unless the second is simpler once the code is read. Test with a re-offer whose first flight is sent before the confirm, asserting the new session plays well under one retransmit interval on a paused or manual clock if the sockets allow it, otherwise by a lower bound on the flight count.

Public API: none. Wire: none.
