# [S] Archive HLS window

## Goal

`moq-hls` lists a replayed recording's whole retained timeline in HLS and DASH
without a server-wide `--window`, while live broadcasts keep a window within
the relay's cache.

## Plan

The export window (`moq_hls::export::Config::window`) trims every broadcast a
server exports. A recording served through `moq_archive::Reader` already
bounds itself: DVR expiry pops records from the replayed timeline, and an
unbounded archive never pops. Today an operator must raise `--window` past the
recording's length, which also inflates segment `Cache-Control: max-age` and
DASH `timeShiftBufferDepth` for every live broadcast on the same server.

Decide how an export learns that a broadcast's timeline is authoritative (the
catalog's `archive` entry carrying a store or replay path, a per-broadcast
option, or a separate server) and cap the derived HTTP and DASH values.
