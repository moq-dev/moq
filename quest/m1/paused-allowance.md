# [S] Any number of streams pause within half the connection window

## Goal

[#9210](https://github.com/moq-dev/moq/pull/9210) merges with its design
checked by the credit model (`rs/moq-net/verify/credit`, the `verify-credit`
skill): each paused stream holds a slot-sized allowance of credit instead of a
stream window, so any number of streams pause within half the connection
window. #9210 deletes this quest.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of the pre-cut flow-control
audit): ✅ #9210 ships in this cut (rejected: landing after the cut to soak).
✅ It merges once the credit model passes on its design (rejected: merging
first and modeling the result).

#9210's own design decisions, settled 2026-11-19, and its two open `Waiting`
items stay on the PR. When the model finds a violation in its design, fix it
there before merging.

The model passes on #9210 at 69dc410be: the window, the cap, and every stream
ending hold (`verify-credit`). Its open finding, `ReceiveCreditAllowance.cfg`,
is #9210's own claim that every pause is charged its slot's allowance. A grant
queued by `Stream::release` can go out before the next lock shrinks the window,
so a pause is charged up to a stream window and a later one can be refused
under the cap. Shrinking in the release's own lock hold passes it; then
promote `EveryPauseFits` and `ChargedAllowance` into the other configs.
