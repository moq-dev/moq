# [S] Resumed copies judge the gap on lite-05/06

## Goal

On moq-lite-05 and 06, an idle resumed copy never misreads a descending
track's newest group as where its feed starts, so readers keep the cached
groups their max age still allows, as they do on lite-07.

## Plan

From [moq-dev/moq#6183](https://github.com/moq-dev/moq/pull/6183), which
fixed lite-07 by judging the gap from SUBSCRIBE_START's first group. On
05/06 the copy still judges it by the first group header, and on a
descending track the first header to land can be the newest group. Their
START has no `largest`, so this needs its own small design within those
versions' wire, which is published and must not change. Propose it in the
PR with options and a recommendation; leaving 05/06 as is, documented, is
one option.

Public API: none. Wire: none.

## Related

- [Resume through an outage](https://github.com/moq-dev/moq/pull/6183) - the lite-07 fix
