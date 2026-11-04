# [S] Re-serving a claimed path keeps the session

## Goal

A JS publisher that answers a claimed path's request with a fresh broadcast
whose first group is above 0 (continuing an earlier broadcast's group
sequence) keeps its session through a relay on `moq-lite-07-wip`; today the
relay closes the publisher's connection with `invalid control encoding`.

## Plan

Seen 2026-11-03 in Kixel's T3 tracks, which worked around it by replacing
tracks in the same broadcast instead.

On 2026-11-04 Kixel's host started releasing and re-serving broadcasts, and
a fresh broadcast starting at group 1 re-served a released path through a
relay on lite-07 at `9a29bc13` without closing the session, so this may no
longer reproduce. First try to reproduce it on `main` with a relay test that
re-serves a path starting at group 1. If it passes, keep the test and delete
this quest. Otherwise find which side encodes or decodes the start group
wrongly and fix it.
