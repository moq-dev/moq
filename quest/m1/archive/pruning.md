# [S] DVR timeline pruning

## Goal

A DVR writer deletes timeline objects that no retained checkpoint needs, so a
long-running DVR stores a bounded number of objects.

## Plan

`moq_archive::Writer` deletes expired group objects but never a
`segments/<segment>` timeline object, so a 24/7 DVR with 2s segments adds
about 43,000 objects a day, and every restart lists all of them. The draft
already allows it: the writer keeps the latest timeline object and enough
earlier groups to recover the retained window from a checkpoint. Delete the
oldest timeline objects no longer needed, one grace period after they stop
being needed, keeping the remaining keys contiguous, as recovery requires.
Extend the restart cleanup and its tests in `rs/moq-archive/src/writer.rs`.
