# [S] Publishers that cannot align refuse a shared epoch

## Goal

The CLI accepts `--epoch` only where two instances fed one stream can
publish identical tracks. It refuses it, naming the reason, for publishers
whose timestamps or bytes come from the instance rather than the input:

- `import capture`, since two encoders never produce the same bytes;
- `import avc3`, which stamps frames from their arrival on stdin
  (`Split::pts`);
- `import rtc --connect`, since each pull's RTP session is its own;
- `transcode`, for the same reason as capture;
- `import archive`, whose replay rewrites catalog and timeline groups under
  the live names, so one name, epoch, and group number could carry
  different bytes.

All five accept it today, so a redundant pair built from them fails at its
first failover instead of at start.

## Plan

Decided 2026-10-13 by the maintainer: same-epoch routes resume each other's
subscriptions, so a publisher that cannot meet the
[same-epoch contract](/quest/m1/hop-aligned-import.md) refuses the epoch
rather than documenting a hazard. `takes_epoch` in `rs/moq-cli/src/args.rs`
returns false for the four import sources, and `validate` refuses `--epoch`
with `transcode`. The archive refusal moved here from
[Replay catalog](/quest/m1/archive/replay-catalog.md) so it lands with the
others, ahead of the same-epoch contract.

Update the `--epoch` paragraph in `doc/bin/cli.md` and the `--epoch` doc
comment to say what accepts it.

Tests: extend `epoch_applies_only_to_a_once_per_run_publisher` with each
refusal, and keep the other sources accepted.

Public API: `--epoch` is refused for these five, where it was accepted.

## Related

- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - the contract the accepted importers meet
- [Replay catalog](/quest/m1/archive/replay-catalog.md) - why archive replay mints its own epoch
