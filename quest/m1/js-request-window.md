# [S] @moq/net grants request IDs as requests close

## Goal

A Rust peer can make any number of requests over one JS session on
moq-transport drafts 14 to 16. Today `js/net` advertises
`MAX_REQUEST_ID = 42069` (`js/net/src/connection/accept.ts`) and never grants
more, so the peer stalls after about 21k requests.

## Plan

Mirror Rust's window from request caps (#4820): advertise a finite window
and send MAX_REQUEST_ID to grant more as requests close. Refuse an incoming
ID past the window with the draft's error, as Rust does. A mocked-time test
drives more than 21k requests through one session.

Public API: none. Wire: behaviour within the drafts; no format change.
