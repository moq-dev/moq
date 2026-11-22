# [M] A widened resumed ordered reader asks its copies' stale give-ups again

## Goal

An ordered reader that resumes across a front's route copies (`rs/moq-net/src/model/resume.rs`, `js/net/src/resume.ts`) and widens its `Delay::max` takes back a gap its serving copy's fill gave up as stale (`Error::Old`) under the narrower budget, unless it already returned that gap, and asks the run again. A plain ordered reader already does (Rust `Cursor::rejudge`, JS `Subscriber.#withdraw`). Rust and JS match.

## Plan

- A copy's subscription is a plain `Subscriber` whose drop reports are final, so a stale give-up it reports lands in the logical record and the `Ordered` gaps for good. A widening needs to reach back through the copy: take the run back from its `Misses`, the logical record's `Report::Dropped`, and `Ordered::gaps`, leaving whatever the reader returned.
- JS skips `#withdraw` for a resumed reader (`#tryDrop` returns before it); mirror the Rust shape there.
- Add the plain reader's reproduction (`a_widened_read_asks_its_own_stale_refusal_again`) for a front in both languages.

Public API: none. Wire: none.
