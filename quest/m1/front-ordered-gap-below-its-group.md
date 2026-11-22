# [M] A front's ordered read returns the gap its copy gave up below the group it reads

## Goal

An ordered reader that resumes across a front's route copies returns a gap for
every run its serving copy gave up, even one given up in the same poll that hands
out a higher group: it goes out before that group, as on a plain reader. Rust
(`rs/moq-net/src/model/resume.rs`, `rs/moq-net/src/model/track.rs`) and JS
(`js/net/src/resume.ts`, `js/net/src/track.ts`) alike.

## Plan

Found on [moq-dev/moq#9437](https://github.com/moq-dev/moq/pull/9437). An
ordered front read hands out group 1200 from a fill that also dropped
`1000..1200`, all landed before the read. The copy queues the drop during the
read, but handing out 1200 marks everything below it delivered
(`Delivered::forget_below`), and `Reader::poll_drop` then discards the run as
covered. A plain reader reports `gap 1000..1200` before `group 1200`; the front
returns `group 1200` alone. JS loses it too, despite `#tryResumed` draining the
copies' drops below the key before `#handOut`.

Draining the reader's queued drops before `forget_below` (as JS does) makes the
Rust case pass but breaks
`waiting_time_start::a_range_before_every_group_reads_past_it`: the viewer's
front then reports `gap 10..21`, a run its relay copy gave up for a time range
before every group, where a direct read returns only group 66. So the fix also
needs a front to report only what its own window owes, as a plain cursor's
window decides, not every run a copy gave up under its local floor and cap.

JS through a nested front holds the same gap back further: with the drops
answered and group 1200 not yet landed, the outer ordered read returns nothing
until the group lands, where Rust and a single JS front return the gap at once.

Reproduce with the reshaped end of `a_widened_read_asks_its_own_stale_refusal_again`
in both languages: answer the fill with group 1200 before the drops, and expect
`[gap 1000..1200, group 1200, gap 1201..1500]` for the front as for the plain
reader.

## Related

- [Switch no-boundary parity](/quest/m1/switch-no-boundary-parity.md) - other work in `model/resume.rs`
