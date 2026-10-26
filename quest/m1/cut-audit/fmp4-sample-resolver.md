# [S] Bound field-less fMP4 runs before sample resolution

## Goal

Parsing a moof cannot expand a field-less `trun` sample count into thousands
of entries per box. Land a bounded mp4-atom representation or explicit
refusal, release it, and update moq-mux to use it.

## Plan

[moq-dev/moq#6864](https://github.com/moq-dev/moq/pull/6864) consolidates
import, subtitle import, and CMAF decode sample resolution. It rejects
empty samples before allocating output, checks disjoint in-bounds ranges,
and explicitly refuses absolute `base_data_offset`. Its resolver allocates
only for samples that own input bytes.

The dependency still expands a field-less run into up to 4096 entries
before that resolver runs. A 16 KB moof of 1000 such runs materializes
4.1 million entries even when the first sample will be refused. Removing
only the output amplification does not close this parser allocation gap.

[kixelated/mp4-atom#248](https://github.com/kixelated/mp4-atom/pull/248)
proposes refusing nonempty field-less runs on decode and encode. An alternative is a counted representation of identical
entries, preserving valid files while breaking the `Trun.entries` API.
The refusal proposal's CI breaks the existing AV1, BBB, and ESDS fixtures
(380 pass, three fail), so the recommendation is the counted representation,
not dropping those valid inputs. Review the API shape before releasing. Do not add another
sample-count cap or duplicate the atom parser in moq-mux.

After adopting the release, run the fMP4 tests and interleaved container
benchmarks, including tiny samples, and confirm any supported field-less
runs stay bounded before the mdat is available.

Public API: depends on the upstream representation. MoQ wire: none.
