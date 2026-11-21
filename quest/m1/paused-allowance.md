# [S] Any number of streams pause within half the connection window

## Goal

[#9210](https://github.com/moq-dev/moq/pull/9210) merges with its design
checked by the [credit model](/quest/m1/credit-model.md): each paused stream
holds a slot-sized allowance of credit instead of a stream window, so any
number of streams pause within half the connection window. #9210 deletes
this quest.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of the pre-cut flow-control
audit): ✅ #9210 ships in this cut (rejected: landing after the cut to soak).
✅ It merges once the credit model passes on its design (rejected: merging
first and modeling the result).

#9210's own design decisions, settled 2026-11-19, and its two open `Waiting`
items stay on the PR. When the model finds a violation in its design, fix it
there before merging.

## Required

- [Credit model](/quest/m1/credit-model.md) - checks the slot allowances and `set_window` before they ship
