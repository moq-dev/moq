# [M] cpp/obs: moq-source.cpp has no test coverage

## Goal

Implement and verify the behavior tracked in [#2860](https://github.com/moq-dev/moq/issues/2860)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

`cpp/obs/test/moq-output-test.cpp` covers the publish path only. Its libmoq stub implements `moq_origin_create`, `moq_origin_close`, and `moq_origin_publish`  -  nothing on the consume side  -  so `moq-source.cpp` is exercised by nothing. `just obs test` reports success without touching it.

That matters more than usual for this file: PR CI never compiles the plugin at all (see `cpp/obs` in the root CLAUDE.md), so `just obs test` is the only automated gate the source path could have, and it currently isn't one.

It bit us in #2856. That PR stopped `moq_net::Client::connect` blocking on the initial announce set, which left `moq_source_start_consume` calling `moq_origin_request` (resolve against what is announced *now*) off the session-connected callback. The announcement had not necessarily arrived, so `on_broadcast` got `Unroutable`, the source blanked, and because consumption is started only for connection epoch 1 nothing ever retried it: OBS stays blank until the user restarts the source. It was caught in adversarial review rather than by a test.

#### Suggested coverage

The stub set needs the consume half: `moq_origin_consume_announced` (+`_close`), `moq_origin_request` (+`_close`), `moq_consume_catalog`, `moq_consume_track`, `moq_consume_close`. The orderings worth pinning, all of which the current build cannot check:

- Connected fires, the announcement arrives *later*, and the source still subscribes.
- A broadcast that is never announced: the wait stays pending and `moq_source_disconnect` closes it, firing the terminal exactly once.
- A reconnect (epoch 2) while an epoch-1 delivery is still in flight: the stale delivery is dropped on the generation check and its handle is closed, not leaked.
- Terminal-callback refcounting: `refs` returns to zero on each of the delivered / errored / closed paths.

The generation and `subscription_ref` bookkeeping in that file is exactly the kind of thing the comments say the build can't verify, which is the argument the existing output test already makes for itself.

Found by the Codex adversarial review on https://github.com/moq-dev/moq/pull/2856.

## Closes

- [#2860](https://github.com/moq-dev/moq/issues/2860) - close this issue when the quest finishes
