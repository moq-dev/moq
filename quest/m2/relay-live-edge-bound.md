# [M] A publisher's live edge across many tracks is bounded in bytes

## Goal

A publisher holding many live tracks, each with one finished group and nothing
more written, cannot make the relay hold more than a bound set by the
relay's own settings. The cache pool's byte budget, which the headroom
governor moves, today reaches only what a writing track sheds; the newest
frame of every live track is kept whatever the budget.

## Plan

Found by the review of the relay's default headroom: `Pool::resize` only
moves the target, a track repays debt when it next writes, a group keeps its
newest frame while open, and the latest group of every track is exempt from
idle expiry. So N tracks that each wrote one large frame and went quiet hold
N frames past any capacity, until the publisher or a subscriber ends them. The
default headroom bounds a track that keeps writing, not this shape.

Decided 2026-11-21 (maintainer, /quest-complete): a per-session byte budget
for live edges (charge a track's retained newest frame to the session that
opened it), so a session past it pays with its own oldest live edges or new
tracks (rejected: reclaiming any idle live edge globally under pressure; a
per-track cap on the newest frame). It confines the cost to the publisher
causing it. Measure it with many tracks of one frame each against a small
headroom on the built binary.
