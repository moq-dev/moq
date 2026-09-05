# [S] js/watch: `<moq-watch>` fails on Firefox ESR

## Goal

`@moq/watch` plays on Firefox 140 ESR, as 0.3.2 did. On 0.5.2 the element
dies in an effect with `TypeError: can't access property "Consumer", (void 0)
is undefined` before playing anything.

## Plan

`(void 0).Consumer` is a module namespace read before that module finished
evaluating, which is what an import cycle looks like under an evaluator
that handles cycles differently from Chrome, or a bundle whose output order
Firefox ESR's module loader resolves differently. It can also be a WebCodecs
or WebTransport feature the ESR line lacks, surfacing as an undefined
namespace after a failed dynamic import. Reproduce on Firefox ESR with the
Svelte page from the issue, bisect between 0.3.2 and 0.5.2, and fix the
cause: break the cycle or gate the feature with the same detection
`@moq/watch/support` already does for other capabilities, so the element
reports what is missing rather than throwing from an effect.

Firefox is not in the browser harness. Add it to `test/wasm`'s Playwright
matrix if the harness can drive it, or record why it cannot.

## Closes

- [#3361](https://github.com/moq-dev/moq/issues/3361) - close this issue when the quest finishes
