# [S] A departed subscription releases its streams

## Goal

On lite-07, when a subscription's last reader leaves, every stream it held
upstream is released, so the credit returns for the next subscription. A
relay test with exactly 3 bidirectional stream slots serves a second
subscription after the first leaves, as it already does on moq-transport-22
and on lite-07 with 4 slots.

## Plan

Found 2026-11-04 by the [scheduler](/quest/m1/stream-credit/scheduler.md)'s
relay test: on lite-07 with exactly 3 bidirectional slots, a second
subscription is never served after the first leaves, while 4 slots work and
moq-transport-22 works with 3. The likely cause is a stream the departed
subscription keeps open upstream (its request stream, or a companion such as
the Tracks stream) after its reader leaves. Reproduce it with the 3-slot
relay test first, find which stream stays open and on which side, fix it, and
set that relay test's credits back to the tight case. With opens now waiting
for credit instead of failing, a leaked stream stalls later subscriptions
silently, and low-credit peers such as browsers hit it first.
