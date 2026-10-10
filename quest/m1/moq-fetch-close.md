# [XS] moq fetch closes its connection cleanly

## Goal

`moq fetch` (`rs/moq-cli/src/fetch.rs`) closes its session before exiting,
so the relay sees a clean close instead of timing the connection out.

## Plan

Found while merging #5174 (2026-10-10), which taught every interop client to
close cleanly and made the harness fail a round on an idled-out connection.
`moq fetch` still drops its connection without `Connection::close` or
`Client::close`; the js-native FETCH_DATA wire-compat cell reproduced the
relay timing it out. Only the wire-compat lanes reach it, and they skip the
idle-out check, so it doesn't fail CI today.

Close the session the way #5174 did for the other clients, on every exit
after connecting, including an error or the `timeout_at` deadline. For the
regression, run a FETCH round with the current relay and the current
`moq fetch` under the idle-out check. No such round exists today (FETCH only
runs in the wire-compat lanes), so add one and wire it into CI. Don't enable
the check for the wire-compat cells: they mix in released relays and clients,
which it excludes because they may not close cleanly or log which connection
closed.

Public API: none. Wire: none.
