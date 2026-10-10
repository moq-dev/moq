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

Close the session the way #5174 did for the other clients, then turn the
idle-out check on for the wire-compat FETCH cells as the regression.

Public API: none. Wire: none.
