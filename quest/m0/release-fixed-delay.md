# [XS] A fixed latency on release ignores the catalog jitter and delay

## Goal

On `release`, which `moq.dev` runs, a viewer who picks a fixed latency
(100 ms, 250 ms, ...) plays at that delay. Today `Sync.#runDelay`
(`js/watch/src/sync.ts` on `release`) adds every rendition's catalog
`delay + jitter` on top of the fixed value, so a publisher whose estimates
ratchet up drags a "100 ms" viewer tens of seconds behind; a user reported a
42 s total buffer with 100 ms selected.

## Plan

`main` already behaves this way since
[#4162](https://github.com/moq-dev/moq/pull/4162), but that change is a
breaking `@moq/watch` rework that won't be backported. Port only the behavior:
when the configured delay is a number, the output delay is that number;
"auto" and "instant" are unchanged.

- The PR targets `release` directly, with no `main` change, and keeps the
  published API so it ships as a patch (see `CONTRIBUTING.md`).
- Add a regression test in `js/watch/src/sync.test.ts` on `release`: a
  registered rendition jitter does not raise a fixed delay.
- The fix reaches `moq.dev` with the next release deploy.
