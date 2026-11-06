# [S] Rename moq-watch's buffer to lookahead

## Goal

`<moq-watch>`'s `buffer` property and attribute, and the `Player`/`Sync`
input behind them, are named `lookahead`. Users read `buffer` as a jitter
buffer and set it on live streams, where it disables catch-up
([moq-dev/moq#8031](https://github.com/moq-dev/moq/issues/8031)). It is
really how far future-dated media, such as TTS written faster than real time,
may run ahead of the live edge. Live slack stays `delay`.

Boundaries: JavaScript only. Nothing in Rust, Python, or the bindings uses
the name. The buffered-ranges UI (`buffer-control.ts`) shows something else
and keeps its name.

## Plan

Planned 2026-11-06 (maintainer, /quest-import of
[moq-dev/moq#8031](https://github.com/moq-dev/moq/issues/8031)).

Decisions:

- Rename: keep `buffer` / ✅ rename in m2. Breaking for `@moq/watch`, so it
  joins the next breaking release.
- Name: ✅ `lookahead` / `ahead` / `burst`. It says what the value is, and
  is already the word the docs use.

Settled by repository rules: no deprecated alias. `buffer` is removed
outright, and `js/watch/MIGRATE.md` gets an Unreleased entry showing the move.
Internal names derived from it (`buffered`, `parseBuffer`) follow when they
mean the same thing. Update `doc/lib/js/watch.md`,
`doc/concept/use-case/ai.md`, the demos, and quests that name the property,
such as [Buffered playback subscribes in ascending
order](/quest/m1/buffered-ascending.md).

Public API: breaking, `buffer` becomes `lookahead` on `<moq-watch>` and the
`Player`/`Sync` inputs. Wire: none.

## Related

- [Buffered audio converges on the shared clock](/quest/m1/audio-jitter-target/buffered-converge.md) - the behavior fix for the same report
