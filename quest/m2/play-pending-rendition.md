# [M] A pending sibling rendition never blanks moq play

## Goal

`moq play` tries renditions in order and waits on each one's broadcast to resolve, so a better-ranked rendition on a sibling broadcast that nothing has announced yet holds back a playable fallback on the catalog's own broadcast, and the window stays blank.
Viewport ordering makes this more likely, since a portrait sibling can now rank first in a portrait window.
A pending broadcast should never block a rendition that can play now, and the preferred rendition should still win once its broadcast lands.

## Plan

- Recommended: while a candidate's broadcast is still pending, open the next candidate that resolves now. Once the pending broadcast lands, pick again and swap make-before-break, the same way a viewport turn does (`Half::turn` in `rs/moq-cli/src/play/playback.rs`), as a new `Switch` variant.
- Rejected: a timeout on each resolve. It needs a timer that AGENTS.md asks about first, and it still blanks the window for that long.
- Check whether `@moq/watch` (`js/watch/src/video/source.ts`) waits the same way, and fix both in one PR if it does.
- Found by CodeRabbit on [#9287](https://github.com/moq-dev/moq/pull/9287).
