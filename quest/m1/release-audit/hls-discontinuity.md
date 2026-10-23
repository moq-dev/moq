# [M] Settle timestamp-reset HLS discontinuity behavior

## Goal

HLS import either supports a timestamp-reset discontinuity with correct
published identity or refuses it explicitly under a documented contract.
Never reuse a track name for a rewound timeline or silently rewrite media
timestamps to hide the problem.

## Plan

[Draft PR6426](https://github.com/moq-dev/moq/pull/6426) checkpoints real fMP4
through the playlist/map path at main c31d7a71f9. Its 16 cases cover audio and
video, unwrapped Legacy and passthrough CMAF, unchanged and changed maps, and
forward and reset discontinuities. Forward discontinuities preserve source
5s/7s stamps under the same track name. Unchanged-map resets fail with a mux
rewind or track timestamp mismatch. Changing the map resource, even to the
same init bytes, creates a different name and preserves the reset timestamp.
These are publication/identity checks, not renderer playback validation.

The matrix also reproduced two ownership defects: replacing a map dropped
the old track unfinished, and a refused passthrough frame dropped its new
group instead of propagating the refusal. The checkpoint finishes a replaced
importer and aborts a refused group with its real error, so consumers can
drain old media or observe the cause instead of `Dropped`. These fixes do not
choose whether timestamp resets should be supported.

Source provenance: same-map importer reuse comes from
[the per-track HLS import](https://github.com/moq-dev/moq/commit/b21b2343f1),
which is in HLS v0.3.0 and later tags. The separate discontinuity hook comes
from [3bd48e3acc](https://github.com/moq-dev/moq/commit/3bd48e3acc), on main
but absent from HLS release tags. The timed-track no-rewind check comes from
[the current identity contract](https://github.com/moq-dev/moq/commit/d73fb3a3c2),
also absent from release and HLS tags through v0.5.16. That tag still allows
rewound group timestamps and clears decode-time state in `seek`; do not
claim this main reproduction proves the same release failure. Release
provenance is source-traced, not an executed released-version reproduction.

Remaining decision: support same-map resets with explicit timeline identity,
or refuse them under a documented scope. Recommend explicit refusal for now:
keep source time and the current no-rewind invariant, retain forward jumps,
and report the unsupported boundary before retrying or partly publishing it.
Support needs a reviewed A/V timeline and catalog/consumer identity contract;
fresh names alone do not demonstrate correct synchronized playback. Do not
assume rebasing or restarting the whole broadcast epoch.

After the maintainer chooses, implement that contract, cover cross-rendition
playback and failure propagation, and update HLS documentation inline. The
checkpoint changes no public API or wire format. Keep the draft until the
policy and remaining consumer behavior are settled.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - decoder identity changes already mint new track names, but same-codec timestamp resets are not covered
