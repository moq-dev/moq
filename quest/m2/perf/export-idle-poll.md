# [M] An export poll nothing woke is cheap

## Goal

A container export poll that no new data woke costs little: today about 24%
of each poll is moq-net track fill and 11 to 19% is catalog polling. A
benchmark shows the idle poll's cost, before and after.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile of the moq-mux TS export tests. It helps those tests and production
exports alike. Profile first, then cut the work an idle poll repeats.

Public API: none expected.
