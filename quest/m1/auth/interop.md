# [S] Interop asserts the AUTH grant

## Goal

`just test interop --all` fails a cell with AUTH (lite-06, or moq-transport
draft-17+ with MoQ Auth) whose client did not receive
the grant its relay token implies, and a new negative round passes only when
a client publishing outside its grant fails loud with `Unauthorized`. Today
the matrix checks media alone, so a malformed AUTH_OK that leaves a session
with no grant passes silently.

## Plan

The Rust and JS interop clients each print the grant they received in one
parseable line; `test/interop/interop.sh`
compares it against the token the cell minted. The negative round mints a
token that excludes the published path and expects the publisher's session
to close with `Unauthorized` naming the path, and the subscriber to see
nothing. Cells without AUTH skip both checks, as do binding clients until
[Bindings](/quest/m1/auth/bindings.md) gives them a grant to print; that
quest adds them to the same assertion. No public API or wire change.
