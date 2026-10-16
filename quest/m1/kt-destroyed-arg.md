# [S] Kotlin refuses a destroyed argument instead of aborting

## Goal

Passing an already-destroyed moq object as an argument to a generated Kotlin
call throws a Kotlin exception; it never aborts the process. Today a
generated converter clones the argument's native handle without taking its
lifetime guard, so a destroyed argument reaches native code and aborts the
JVM. A regression test at a constructor boundary fails without the fix, and
valid closed-publication behavior is unchanged.

## Plan

Found while landing [#5559](https://github.com/moq-dev/moq/pull/5559).

- ✅ Decided 2026-10-15 (maintainer): fix it in the Kotlin template of the
  generator fork moq pins today and bump that pin; do not hand-patch the
  generated bindings. The [hard fork](/quest/m1/uniffi-fork.md) carries the
  fix when it moves the generators.
- Reproduce first, at a constructor that takes another moq object (for
  example a handle built from a broadcast), with the argument destroyed
  before the call. Check whether the same converter path serves methods and
  free functions, and cover each shape the template emits.
- The guard check belongs where the converter lowers an object argument, the
  same place a receiver already checks its own guard.
- Verify that a closed but not destroyed publication still behaves as today:
  its calls return the closed error, not the new destroyed one.
- Check whether the other generators moq uses (Swift, Python, Go, Dart, C++)
  lower object arguments the same way; fix any that share the bug in the same
  pin bump, or record them as follow-ups.

Public API: a destroyed argument becomes an exception instead of an abort.
Wire: none.

## Related

- [Hard-fork the binding generator](/quest/m1/uniffi-fork.md) - inherits the template fix
