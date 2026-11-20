# [M] Push-to-talk plays on the native listener again

## Goal

Nightly's Audio quality push-to-talk step passes its four native-listener
checks again, with the cause found and fixed rather than a ceiling moved.

## Plan

Found by [moq-dev/moq#9139](https://github.com/moq-dev/moq/pull/9139), which
saw the push-to-talk row fail on 2026-11-18 (run
[44342367698](https://github.com/moq-dev/moq/actions/runs/44342367698)); it failed again on 2026-11-19
([44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083)) and passed on 2026-11-17. All four native
checks fail together and the native metrics come back null, so the analyzer
cannot line up the native clock with the page's:

```
native: an utterance did not resume on its head; ... no playback from the first press; ...
"native": {"offset": null, ... "comfort": null}
```

Null metrics suggest the native listener played nothing, not that it played
badly. It broke between the 2026-11-17 and 2026-11-18 heads; bisect that
range first. Suspects that touch the native or publish audio path:
[moq-dev/moq#8984](https://github.com/moq-dev/moq/pull/8984),
[moq-dev/moq#9027](https://github.com/moq-dev/moq/pull/9027), and
[moq-dev/moq#9031](https://github.com/moq-dev/moq/pull/9031). Reproduce
locally with the Audio quality job's push-to-talk step; never dispatch
Nightly.

Caused by: whichever PR the bisect names.

Public API: none expected. Wire: none expected.
