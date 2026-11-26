# [M] A widened read gets history its budget folded before the group arrived

## Goal

A subscription that adds a range, or widens a budget, behind what it already read gets every group the new range owes, including one that reaches the track only afterwards, such as a relay's fill answering a lookback it never cached. Today `Cursor::forget_past_budget` (`rs/moq-net/src/model/track.rs`) and `Subscriber.#forgetPastBudget` (`js/net/src/track.ts`) fold a gap into `delivered` "whether this track holds it or not", so a group landing there later is skipped as already handed out. Only cached groups no range covers are kept back from the fold (`lowest_uncovered`, `#lowestUncovered`), and `Cursor::rejudge` re-asks only misses a fill gave up as stale. Rust and JS match.

## Plan

- Reproduce on a relay: a reader at the live edge through a relay holding only its newest groups adds a lookback range, and the origin's answer for the older group arrives after the update. The same update already delivers it in-process and through a relay that cached it.
- Keep a folded gap that no range or budget judged reopenable, instead of keeping the gap out of the fold: never folding below a live read's start holds that gap open for the read's lifetime, so with `forget_below`'s prefix fold every run above it piles up. One shape: fold judged gaps with `insert_run` and leave the unjudged ones open, or remember what the fold covered unjudged and reopen it when `admit` gains it.
- Once a widening reopens what it owes, drop `lowest_uncovered` / `#lowestUncovered`, which only cover the cached case.

Public API: none. Wire: none.

## Related

- [Widened front reader asks stale again](/quest/m3/widened-front-reader-asks-stale-again.md) - the same widening for a resumed reader's stale give-ups
