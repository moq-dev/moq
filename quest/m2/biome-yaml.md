# [XS] Format YAML with Biome

## Goal

`just check` fails on unformatted YAML, and `just fix` formats it: Biome's
YAML formatter is on with two-space indentation for the workspace's 72
tracked YAML files (workflows, Dart, packaging, Read the Docs). Generated or
vendored YAML stays excluded.

## Plan

Set `yaml.formatter.indentWidth: 2` (YAML cannot use the global tabs) and
reformat in the same PR. Biome double-quotes single-quoted strings and
indents sequences under their key, with no option to keep them flush; accept
that style. Check that the Dart tools (`dart analyze`, `pub`), nfpm, and the
workflow tests still read the reformatted files.

Decided with the maintainer on 2026-10-29: adopt Biome's YAML formatter
(✅ yes / no), in m2, as in moq.pro.

## Required

- [Upgrade Biome to 2.6](/quest/m2/biome-2-6.md) - the workspace runs a Biome
  with the YAML formatter

## Related

- [moq-dev/moq.pro `quest/m2/biome-yaml.md`](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/biome-yaml.md) -
  the same switch in moq.pro
