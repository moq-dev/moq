# [S] C consumers on the generated API

## Goal

The C interop client (`test/interop/clients/c`) and the `doc/lib/c` samples use
the generated `moq-c` API and pass `just test interop --all` and the doc-sample
build. Nothing in the repository calls the hand-written ABI any more.

## Plan

- Port the interop client first; it is the smallest real consumer and proves
  the callback and dispatcher shape end to end.
- Rewrite `doc/lib/c` around the generated API, including a sample that runs
  callbacks on the host's own loop through `moq_set_dispatcher`.

## Required

- [moq-c package](/quest/m1/c/package.md) - the package these consumers link
