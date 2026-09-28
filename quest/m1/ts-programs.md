# [L] moq import ts: select programs

## Goal

A multi-program transport stream is never silently merged. Today `import ts`
keeps the first program's identity, adds every PMT's streams to one broadcast
on one clock, and loses content; `export ts` rebuilds one program. After
this quest `import ts` refuses a multi-program input by default, naming the
programs, and `--program` chooses what to import.

## Plan

Decided:

- Default: error before publishing when a PAT (initial or later) lists more
  than one non-zero program, naming each program and pointing at `--program`.
- `--program <n>` imports program `n` only.
- `--program all` publishes one broadcast per program, named
  `<broadcast>/<program_number>`, each with its own clock and catalog.
- `export ts` stays one program per broadcast.

Tests: a synthetic two-program input with far-apart clocks fails naming both
programs; `--program 2` publishes only program 2's streams on program 2's
clock; `--program all` publishes both broadcasts, each timed correctly. Update
`doc/bin/cli.md` and the `--help` text.

## Closes

- [#4353](https://github.com/moq-dev/moq/issues/4353) - close this issue when the quest finishes

## Related

- [TS import shared shift](/quest/m1/ts-import-shared-shift.md) - its per-program shift assumes the single program this settles
