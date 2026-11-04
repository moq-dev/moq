# [S] Re-serving a claimed path keeps the session

## Goal

A JS publisher that answers a claimed path's request with a fresh broadcast
whose first group is above 0 (continuing an earlier broadcast's group
sequence) keeps its session through a relay on `moq-lite-07-wip`; today the
relay closes the publisher's connection with `invalid control encoding`.

## Plan

Seen 2026-11-03 in Kixel's T3 tracks
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)), which
worked around it by replacing tracks in the same broadcast instead. Find
which side encodes or decodes the start group wrongly, fix it, and add a
relay test that re-serves a path starting at group 1.
