# [S] HLS export drains after End

## Goal

After a broadcast's announce `End`, the HLS exporter still fetches the init
and segments its tracks retain, so a recorder drains a cleanly ended
broadcast. Today `Rendition::track()` (`rs/moq-hls/src/export/rendition.rs`)
looks the track up again for every fetch, and a closing broadcast refuses that
(`Unroutable`). A replacement must still never serve its bytes under the old
rows.

## Plan

Retain the bound `moq_net::track::Consumer` before `End` and use it for both
init bootstrap and segment fetch. `poll_sync` clears the window on any close
because a clean end and a replacement look the same; define how a clean `End`
differs from a replacement before retaining rows after close. Add regression
tests: init and segment fetch after a clean end, and replacement refusal. A
finite cutoff for a `Restart` drain is separate work.

The clean-End regressions now cover init bootstrap and segment fetch for both
the catalog broadcast and a sibling. Production behavior is unchanged while
the following maintainer choice is unresolved:

1. Recommended: allow retained old-instance media to drain after both `End`
   and `Restart`, without ever resolving replacement media. This satisfies the
   Goal, but changes the stronger replacement-refusal requirement above. Suggested
   wording: "End and Restart may drain retained old-instance media; never resolve
   or serve replacement media under old rows. A finite Restart-drain cutoff is
   separate."
2. Keep strict refusal after `Restart`. This needs lifecycle provenance captured
   atomically with the resolved broadcast, including already-pinned sources and
   leaves served under a standing dynamic prefix. A separately created
   `Source::follow` watcher cannot establish that provenance on its own.

Existing replacement-refusal tests remain intact. Do not change that behavior
until the maintainer settles this choice.
