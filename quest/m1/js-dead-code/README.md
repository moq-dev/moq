# JS code and manifests carry no dead weight

## Goal

The JS workspace holds no export, class member, or dependency that nothing
uses, and no import its package does not declare, and a gate check keeps it
that way. The epic has no work of its own: it completes when every child
merges.

## Plan

A trial of [skif](https://github.com/polylint/skif) 0.9.0 on 2026-11-17
found the work below. skif itself was too young to adopt (16 days old, four
releases in two days, one of them breaking), and its config-free run was
mostly false positives: shell-launched entry points, `@typescript/lib-dom`,
`Bun.resolveSync` targets, and Rust test data. It also missed `zod` in
`test/interop/clients/js-native`. Each finding the children list was
re-checked by hand.

Decided 2026-11-17 (maintainer, /quest-plan):

- ✅ fix the findings once now, and evaluate other dead-code tools in case
  skif is too immature (rejected: fixes plus a gate check in one quest;
  folding the findings into the [code audit](/quest/m1/audit-code.md)).
- ✅ the tool evaluation adopts the winner in the gate (rejected: evaluate and
  report only; an on-demand recipe the code audit runs).
- ✅ `js/common` becomes a workspace (rejected: declaring its imports at the
  root; leaving it).

Placed in m1: internal cleanup and a check, none of it breaking. None of the
exports below reaches a package's `exports` entry, so no public API changes.

## Related

- [Code audit](/quest/m1/audit-code.md) - asks for a dead-code tool given every real entry point, which the gate child sets up
- [Unused public API](/quest/m2/unused-api/README.md) - the same rule for published items; this epic covers module-internal ones
