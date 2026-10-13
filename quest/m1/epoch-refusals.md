# [S] Publishers that cannot align refuse a shared epoch

## Goal

The CLI accepts `--epoch` only where two instances fed one stream can
publish identical tracks. It refuses it, naming the reason, for publishers
whose timestamps or bytes come from the instance rather than the input:

- `import capture`, since two encoders never produce the same bytes;
- `import avc3`, which stamps frames from their arrival on stdin
  (`Split::pts`);
- `import rtc --connect`, since each pull's RTP session is its own;
- `transcode`, for the same reason as capture.

All four accept it today, so a redundant pair built from them fails at its
first failover instead of at start.

## Plan

Decided 2026-10-13 by the maintainer: same-epoch routes resume each other's
subscriptions, so a publisher that cannot meet the
[same-epoch contract](/quest/m1/hop-aligned-import.md) refuses the epoch
rather than documenting a hazard. `takes_epoch` in `rs/moq-cli/src/args.rs`
returns false for the three import sources, and `validate` refuses
`--epoch` with `transcode`. Archive replay's refusal belongs to
[Replay catalog](/quest/m1/archive/replay-catalog.md).

Update the `--epoch` paragraph in `doc/bin/cli.md` and the `--epoch` doc
comment to say what accepts it.

Tests: extend `epoch_applies_only_to_a_once_per_run_publisher` with each
refusal, and keep the other sources accepted.

Public API: `--epoch` is refused for these four, where it was accepted.

## Related

- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - the contract the accepted importers meet
- [Replay catalog](/quest/m1/archive/replay-catalog.md) - refuses `--epoch` for archive replay
