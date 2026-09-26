# [XS] Archive refuses backward timestamps

## Goal

A resumed recording refuses a track whose timestamps go backward past the
recovered timeline, failing loud like the group-ID check, instead of writing
overlapping media time. The caller starts a new prefix.

## Plan

Check the first group's timestamp against the recovered track's last recorded
timestamp at enrollment, and test both a backward and a forward restart.

## Required

- [Archive](/quest/m1/archive/README.md) - the recovery this hardens ships with the line
