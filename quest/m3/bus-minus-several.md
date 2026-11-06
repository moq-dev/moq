# [S] A mix-minus removes several sources

## Goal

A `bus` mix-minus takes out any number of sources, so a guest's return feed
can drop every copy of that guest's own sound.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`Mix::minus` (`rs/moq-audio/src/bus.rs`) takes one `Strip`. A studio guest
with a camera and a screen share has two sources of their own sound, so their
return feed echoes one of them ([moq-dev/studio#37](https://github.com/moq-dev/studio/pull/37)). Change `minus`
itself rather than adding a second method.
