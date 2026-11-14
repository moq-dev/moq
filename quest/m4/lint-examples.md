# [XS] PR CI builds the C++, Go, and Python interop clients

## Goal

PR CI builds the C++, Go, and Python interop clients
(`test/interop/clients/{cpp,go,python}`) when their bindings or the client
change, so a binding API change that breaks one fails the branch rather than
the next interop run.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session): the
clients went stale unnoticed while
[moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311) changed the
decoder API, since only the interop runs build them. A build (and Python
import or type check) is enough; running them stays with interop.

Today no module in `sh/dispatch.sh` matches those directories (the `go`,
`cpp`, and `py` modules cover only `go/`, `cpp/moq/`, and `py/`), so a change
there or a bindings change selects nothing that builds them. Widen the
matching modules, or add a step to them, so each client builds when its
binding or its own directory changes.

The Rust half of the original quest landed in
[moq-dev/moq#7786](https://github.com/moq-dev/moq/pull/7786): the gate's
`just rs test` adds `--benches --examples` (`sh/rs/select.sh`), so the Rust
interop clients and every example are linted on the branch.

Public API: none. Wire: none.
