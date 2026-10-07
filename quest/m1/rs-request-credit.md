# [S] Rust requests fail fast without stream credit

## Goal

A moq-net request (subscribe, track info, fetch, announce interest; lite and
moq-transport) whose stream open finds no stream credit fails at once with a
local error, instead of waiting for the peer to grant more. A relay resetting a
downstream stream because of it sends `Internal`. Publisher group streams are
out of scope.

## Plan

Decided 2026-10-07 with [JS requests](/quest/m1/js-request-deadline.md):
waiting for credit queues work the caller can't see, so a request is refused
rather than parked. Poll the open once; `Pending` means no credit, and
dropping the open cancels it cleanly. Rust gets no answer timer: a request
lives until it is answered, its demand leaves, or the session closes, and a
relay shouldn't kill a subscribe on a slow upstream hop.

Propose the error variant's name in the PR. Public API: one new `Error`
variant. Wire: none.
