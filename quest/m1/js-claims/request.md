# [S] A claimed path resolves through origin.request()

## Goal

Through a relay on `moq-lite-07-wip`, a JS reader's `origin.request(path)`
for a path a JS publisher claimed and answers on demand resolves to the
served broadcast, as `origin.follow(path)` already does.

## Plan

Seen 2026-11-03 in Kixel's T3 tracks
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)): `follow()`
worked and `request()` never resolved. Reproduce with a JS publisher and a
JS reader through a `moq-relay` on lite-07, find whether the relay or the JS
origin drops the request, fix it, and keep the test.
