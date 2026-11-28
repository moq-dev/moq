# [M] A widened resumed ordered reader asks its copies' stale give-ups again

## Goal

An ordered reader that resumes across a front's route copies (`rs/moq-net/src/model/resume.rs`, `js/net/src/resume.ts`) and widens a live join's `max` takes back a gap its serving copy's fill gave up as stale (`Error::Old`) under the narrower budget, unless it already returned that gap, and asks the run again. A plain ordered reader already does (Rust `Cursor::rejudge`, JS `Subscriber.#withdraw`). Rust and JS match.

## Plan

Moved from `m1` to `m3` on 2026-11-24 after closing [moq-dev/moq#9437](https://github.com/moq-dev/moq/pull/9437):

- `#9437` added a complex `set_withdrawing` / `take_withdrawn` / `settle_gap` / `Logical::take_back` untelling layer across route copies, which hit three unresolved bugs in review (unbounded settlement records on retained copies, discarded queued loss notifications, and settled gaps missing copies created by a subsequent route switch), plus duplicated `#9529`'s `quest/m1/front-drop-with-next-group.md`.
- Lite-07 `SUBSCRIBE_DROP(OLD)` now carries the judged `Max Age` (replacing `SubStream::stale_budget`, [moq-dev/moq#9719](https://github.com/moq-dev/moq/pull/9719)): carry the judged `Max Age` cleanly through `resume::Reader` / `Logical::record` (in Rust and JS) instead of building a copy-wide untelling layer, and ensure returned gaps stay excluded across route switches without unbounded per-copy settlement state.
- Add the plain reader's reproduction (`a_widened_read_asks_its_own_stale_refusal_again`) for a front and nested front in both languages (and check that returned gaps remain settled across switches).

Public API: none. Wire: none.
