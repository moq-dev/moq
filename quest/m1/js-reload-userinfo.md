# [XS] JS redirect keys strip URL userinfo

## Goal

js/net's reconnect (`js/net/src/connection/reload.ts`) keys an endpoint
without its URL userinfo, as Rust does, so a redirect loop is detected
whatever credentials the URL carries and none of them end up in the key.

## Plan

`endpoint()` clears only `search` and `hash` today. Rust's redirect-key rule
from [moq-dev/moq#6162](https://github.com/moq-dev/moq/pull/6162) also drops
userinfo; match it exactly, and test both a userinfo URL and a redirect loop
that differs only in userinfo.

Public API: none. Wire: none.

## Required

- [Rust redirect loops](/quest/m1/tokio-redirect-loop.md) - lands the Rust rule this mirrors
