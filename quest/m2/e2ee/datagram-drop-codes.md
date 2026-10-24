# [S] Typed codes for dropped datagrams

## Goal

A received datagram that fails any open check is dropped with an event carrying the draft's typed code, and the track continues, in the draft and in both cores:

- a sequence above `2^53-1` is `identity`;
- a payload shorter than the tag or longer than the datagram cap is `oversize`;
- a bad tag stays `authentication`.

Today the draft's Failure Behavior names only the bad tag, and since [#6622](https://github.com/moq-dev/moq/pull/6622) both cores fold the first two into the `authentication` event, so an identity or size failure masquerades as an AEAD failure.

## Plan

Decided with the maintainer on 2026-10-24:

- The event carries its typed code rather than `authentication`, so the draft's failure taxonomy holds on the receive path too.
- Add `Identity { sequence }` and `Oversize { sequence }` beside `Authentication` and `Duplicate` in Rust's `datagram::Event` (`rs/moq-e2ee/src/datagram.rs`) and TS's `Event` (`js/e2ee/src/datagram.ts`), one variant per draft code, matching the existing pattern. Rejected: one `Dropped { sequence, failure }` variant, a bigger break for no behavioral gain.
- Draft: Failure Behavior gains one normative rule, that any datagram open failure (`identity`, `oversize`, `authentication`) MUST drop that datagram and emit its code while the track continues; only `exhausted` ends the read. Add a matching `-01` changelog bullet.
- Move #6622's `2^53` sequence, oversize, and truncated datagram tests in each suite to assert the new codes, and update the doc comments in `datagram.rs` and `js/e2ee/src/track.ts` that describe the folding.

[#6623](https://github.com/moq-dev/moq/pull/6623) also edits the draft, in Key Derivation and Security Considerations; if it lands first, merge main and expect only a changelog conflict.

Public API: two new datagram event variants in each core, breaking exhaustive matches; the Impact bullet says to handle them like `Authentication`. Wire: none.
