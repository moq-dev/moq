# [M] The Python and Go publishers' tone reaches every subscriber without a timestamp gap

## Goal

The interop matrix's `python -> *` and `go -> *` cells pass their tone check every run, alone and
under load, fixed at the cause, without loosening `tone.py`'s continuity bound.

Seen once, in a local `just test interop --all` on
[moq-dev/moq#9720](https://github.com/moq-dev/moq/pull/9720) (head `a8bb04c313`, after merging
`main` at `54c862e955`): `python -> cpp`, `python -> c`, `python -> gst`,
`go -> js-native-node`, and `go -> js-native-bun` failed `tone.py`'s
`timestamp gap: a frame at ..., the previous one ended at ...`. The rerun passed, and that PR
touches no audio or publish timing. Its follow-up note in the PR body is the only record; the
failing run's logs were not kept.

## Plan

What CI shows: none of the twelve scheduled Interop runs from 2026-11-15 to 2026-11-26 failed a
tone check, by timestamp gap or by `audio gap`. The last audio failures were on 2026-11-12
([43260674067](https://github.com/moq-dev/moq/actions/runs/43260674067), `go -> python`,
`audio gap after 648 samples`) and 2026-11-13
([43457651679](https://github.com/moq-dev/moq/actions/runs/43457651679), `js ->` cells).
Those were the reader's own `Gap`, which
[moq-dev/moq#8611](https://github.com/moq-dev/moq/pull/8611) and
[moq-dev/moq#8633](https://github.com/moq-dev/moq/pull/8633) worked on, not `tone.py`'s
timestamp check. So this is rarer than one per scheduled run, and seen so far only on a loaded
local host.

Both publishers stamp their tone by samples written (`_publish_tone` in
`test/interop/clients/python/interop.py`, and `publishTone` in `test/interop/clients/go/main.go`
since [moq-dev/moq#9296](https://github.com/moq-dev/moq/pull/9296)), so their stamps stay
contiguous even when a starved runtime writes late. A gap in a subscriber's decoded stamps means
audio went missing after the write: in the binding's encoder (`moq-audio` through `moq-ffi`), in
delivery, or in the reader. The failing cells span five subscribers but only those two
publishers, so start on the publisher side.

1. Reproduce on current `main`, which since
   [moq-dev/moq#9561](https://github.com/moq-dev/moq/pull/9561) cuts audio groups at 100 ms.
   Loop `just test interop --publishers python,go,rust --subscribers cpp,c,gst,js-native-node,js-native-bun,rust`
   (`rust` on each side as the control) through `quest gate`, under CPU load such as `stress-ng`
   from `nix shell nixpkgs#stress-ng`, and record the failure rate. Keep each failing run's
   harness directory: the decoded `<prefix>.<codec>.txt` and the relay log.
2. Read the gap's size off the `.txt`. One 2.5 ms Opus frame or one 20 ms write batch points at
   the publisher's encoder path; a whole audio group points at a group the relay or the reader
   skipped, which the relay log's `moq_net::lite::publisher` lines for that group should show.
   Log the Python tone's write lateness the way the Go client does since #9296 if the timing
   matters.
3. If nothing reproduces after a fair sweep, record the counts here and make a failing cell print
   the gap's size and its group, so the next hit decides it.

Public API: none. Wire: none.
