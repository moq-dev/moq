# [S] Plan: a generation identity for cacheable media URLs

## Goal

A settled answer to where a generation ID comes from, so
[HLS generation](/quest/m2/hls-generation.md) can start. The broadcast epoch
was going to supply it;
[moq#3225](https://github.com/moq-dev/moq/pull/3225) removed Epoch from the
drafts as spec-only and never implemented, retired
`draft-lcurley-moq-broadcast`, and made announcements prefix routes with no
per-broadcast identity to hang a generation on.

## Plan

Deliberately unsettled after a 2026-09 planning pass. The open question is
which layer owns the identity at all: moq-net, hang, or only a managed
deployment (moq.pro) that already keys recordings itself. Until that is
decided this quest stays a plan quest; do not start
[HLS generation](/quest/m2/hls-generation.md) against a guess.

What the pass settled, so it is not re-derived:

- The requirement is unchanged: a CDN edge needs one URL to mean the same
  bytes on every edge, so the ID has to be minted by the publisher and travel
  with the content, never a wall-clock or a relay-local counter.
- The two collision sources stay distinct. A live rendition reconfigure under
  the same track name is not banned by any rule (MSF forbids it, hang does
  not) and `moq-hls` already tears the pooled rendition down and rebuilds it
  when the catalog config changes, so only the URL reuse remains there. A
  publisher restart is the harder half: `moq-net` splices a re-attached
  source into the existing broadcast by hop ID so consumers never observe
  it, and group sequences restart at 0.
- An epoch on ANNOUNCE_START / ANNOUNCE_UPDATE is rejected. Announcements are
  capability routes, and a `transcode/*` route followed by a more specific
  `transcode/foo` route must keep stitching by hop ID; a per-route epoch would
  break that. `main`'s copy of `draft-lcurley-moq-lite.md` still carries the
  old Epoch text on those messages and on TRACK, TRACK_INFO, SUBSCRIBE, and
  FETCH; `dev` deleted all of it in [moq#3225](https://github.com/moq-dev/moq/pull/3225),
  and `dev` is the draft this decision is made against. It was never
  implemented on either branch.
- The candidates left, none chosen: an `Epoch` field on TRACK_INFO stamped
  from `broadcast::Info` (per-broadcast value, per-track carriage, fits the
  existing rule that TRACK_INFO is fixed for a track's lifetime, and a
  takeover across epochs would end the subscription instead of splicing); a
  hang catalog root `generation` (no wire change, routing-blind, so a
  restart's groups can land on the old handle before the new catalog); an
  epoch segment in the broadcast path under prefix routes (relays need
  nothing, every consumer and token must resolve the newest, random values do
  not order); or no protocol identity, with the managed edge minting one per
  publisher session downstream.
- If a wire epoch is chosen it folds into lite-06-wip, is minted at random
  on `broadcast::Info` with an application override so an archive replays a
  recording under its recorded value, and reaches moq-transport as a
  SUBSCRIBE_OK parameter.
