# [S] A subscribe after replacement reaches the new track

## Goal

Through a relay on `moq-lite-07-wip`, a subscribe sent right after a JS
publisher replaces a track in a claimed broadcast (continuing its group
sequence) reaches the replacement instead of failing `Unroutable`.

## Plan

Seen intermittently 2026-11-03 in Kixel's T3 tracks
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)), whose
failed sources replace their track in place. Reproduce it with a relay test
that replaces a track and subscribes immediately, many times. Check first
whether [front parking](/quest/m2/origin-front-parks.md) covers it; if it
does, rank that quest into m1 instead of fixing it twice.
