# [S] Plan: shared coordinates for redundant TS publishers and exporters

## Goal

Settle what shared information makes redundant TS publishers and exporters
identify the same content and schedule. Rewrite the implementation quests
around that contract before promising failover or ST 2022-7 compatibility.

## Plan

The 2026-10-11 audit found two missing prerequisites:

- Same epoch identity does not establish a source PCR/PTS-to-wall mapping,
  a 33-bit timestamp wrap era, or a common raw-packet position. Importers
  starting at different offsets cannot infer these from the UUID alone.
  `catalog::Timebase::place` applies a supplied mapping; it does not discover
  one. Raw fallback's timeout cuts also depend on acquisition history, even
  though the CLI currently accepts a shared `--epoch` there.
- Identical TS packets are insufficient for ST 2022-7, which requires the
  matching RTP packets, including headers. Complementary losses on redundant
  paths are a reason to merge them; requiring the two legs to drop the same
  content defeats that benefit.

Present the maintainer with the actual shared-coordinate choices: an
explicit common source anchor and era, a common upstream packetizer, or an
honestly narrowed importer contract. Include what each can prove when the
standby starts after a timestamp wrap, a clock break, or a fallback group cut.
Do not derive a media clock from wall arrival and call it deterministic.

For egress, separate deterministic TS slot/counter identity for common
content from coordinated RTP packetization, sequence/timestamp/SSRC identity
and any receiver merge. Decide the intended scope and API before implementing
`--sync`. NTP-synchronized hosts alone do not supply the missing source map.

Produce focused implementation quests and explicit refusals for unsupported
shared-epoch modes. Their automated tests start peers at different source
offsets, cross wraps, vary delivery skew, and introduce complementary loss;
compare common content in its shared coordinate rather than whole files
whose delivered content differs. Actual ST 2022-7 claims require RTP-level
tests against the [standard](https://pub.smpte.org/latest/st2022-7/st2022-7-2019.pdf).

## Related

- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - currently assumes an input-derived wall anchor is available
- [Deterministic TS legs](/quest/m2/ts-hitless.md) - TS identity and RTP recovery need separate acceptance evidence
- [TS passthrough export](/quest/m2/ts-passthrough-export.md) - unchanged packets do not establish identical groups from independent starts
