# [M] A restarted instance cannot reuse group sequences

## Goal

A publisher restarting under the same instance at a higher epoch, which `js/net/src/origin.ts` splices onto the existing logical front (and Rust's origin, `rs/moq-net/src/model/origin.rs` with `front.rs` and `resume.rs`, likewise continues), is refused loudly when its sequences go backwards. Today resume silently drops its groups as already delivered, or a recovering reader stitches frames from two streams into one group.

## Plan

Found by the 2026-11-07 invariant audit (model-7, js-10). The recreated-track half (model-2, js-3) and the datagram exemption landed in [moq-dev/moq#8202](https://github.com/moq-dev/moq/pull/8202): a broadcast keeps one high-water record per name, a replacement track refuses sequences below it with `Error::Reused` (JS `Reused`), and an application seeds past it with `create_group`.

Decided 2026-11-07 (maintainer, invariant-audit planning): reuse is misuse, so the library prevents it and never seeds silently.

What #8202 learned about the splice:

- The Rust front's driver records each source's instance and table clock (`instances` in `origin.rs`), not its epoch, so a splice cannot tell a restart from a sibling route of the same epoch. A sibling route may lag a few groups, so a lower largest there is legitimate; only an epoch increase promises the new copy continues past everything the old one served.
- A copy knows its upstream's largest only from lite-07's START (`report_largest`), or live for an in-process source. A restart that has produced nothing when it is spliced reports none. Its groups below the readers' resume point never cross the wire, and `Recover` waits on the new copy's group of the same sequence. So a splice-time check alone misses exactly the stitching case.
- `RouteOptions::with_instance` documents that a restart must continue the sequence space, so a publisher restarting in-process on the same origin could share the earlier epoch's per-name record, the mechanism #8202 added for recreated tracks, and refuse at `create_group` time.

## Waiting

- Maintainer: pick the detection. (a) Splice-time check: on an epoch increase, abort the logical track with `Reused` when the new copy's largest is below the newest group a reader handed out. Misses a restart with no group yet. (b) Also have each resume reader watch a restarted copy's largest until it passes the reader's newest group. This still misses a remote restart whose low groups never cross the wire. (c) Source side: an origin shares the per-name record across epochs of one path and instance, so an in-process restart refuses reused sequences at creation. A cross-process restart must persist its sequences along with its epoch. Recommendation: (c) plus (a). (c) covers the case the library can know, at the source. (a) catches a relay seeing a remote restart that already published below. Document (b)'s gap rather than build it.

## Related

- [A job's exact route continues its claim's readers](/quest/m2/announce-takeover.md) - instance continuity across a claim's jobs; [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) changes the same driver state
