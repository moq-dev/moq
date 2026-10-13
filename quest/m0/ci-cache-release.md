# [XS] A stable cache server enforces the CI write policy

## Goal

Condition: a stable `jdx/mr-boxington-cache` release supports anonymous reads
and server-enforced GitHub OIDC writes restricted to the main cache warmer,
as [Self-hosted CI](/quest/m0/self-hosted-ci.md) requires.

## Plan

Decided in the quest audit: wait for a stable release rather than select an
unreleased pin or weaken the write boundary. The latest stable release,
v0.1.1, allows writes in anonymous mode; [#5218](https://github.com/moq-dev/moq/pull/5218)
therefore keeps workflow routing hosted while preparing the runner module.

Check the upstream releases and their authentication policy. Advance this
condition by identifying a stable release that supports the required split;
record its version and policy evidence when deleting this quest. The
self-hosted CI quest owns packaging it and testing that anonymous and PR-ref
writes fail while the main warmer can write. Keep `CI_RUNNER` unset until
that implementation and its validation are complete.

## Related

- [Self-hosted CI](/quest/m0/self-hosted-ci.md) - packages and proves the cache policy before enabling workflow routing
