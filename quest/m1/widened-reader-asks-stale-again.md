# [M] A widened Rust ordered reader asks its own stale give-ups again

## Goal

When a Rust `track::Ordered` reader widens its `Delay::max`, a run its own fill gave up as stale (`Error::Old`) under the narrower budget is no verdict for it anymore, unless the reader has already returned it as a gap. The reader asks that run again, and the publisher answers under the wider budget, as js/net already does.
This is maintainer decision 1 on [moq-dev/moq#9172](https://github.com/moq-dev/moq/pull/9172#issuecomment-7026835526) applied to a reader's own refusals: a drop judged under a narrower budget never settles a wider reader.

Today a Rust reader keeps the give-up. Reproduce on a received copy holding 900 and 2000, a stale drop of 1000..1500 under 10s, and keys 901..1000 still owed. An ordered reader of 900..1600 with a 1s budget reads 900 and asks a fill for 1000..1500, and the fill refuses it with `Old`. The reader then widens to `None`, and 901..1000 is skipped. The reader returns `Gap(1000..1500)` and never asks again. js/net, given the same steps, holds and asks again for 901..1500.

## Plan

- The give-up lives in three places, and each needs a stale budget to reverse it: `Misses::given_up` (`track/miss.rs`, which `give_up_stale` fills with the asked budget), the record's `reported` runs (`record.rs`, moved there when `Ordered::find_gap` polls the drop), and `Ordered::gaps`, which holds a gap found but not returned yet.
- On a budget that widens past a run's budget, withdraw it from all three: the queued gap, its `Report::Dropped`, and the `given_up` run along with its `judged` keys. The next judgement then asks for it. A gap already returned stands, since a read in key order never goes back.
- Keep #9172's time-start barrier (`Misses::unanswered`) in step: it counts a given-up run as accounted, which is right only while that run stays given up.
- Mirror check: js/net's `refused` runs carry their stale budget, and `#verdicts` skips a narrower one. Port the reproduction to `ordered.test.ts` so both stay pinned.
