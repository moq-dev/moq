# [S] mp4-atom dOps channel mapping

## Goal

A released `mp4-atom` decodes and encodes a `dOps` box with any channel mapping
family, exposing the family and its table (stream count, coupled count, and the
per-channel mapping) instead of refusing a nonzero family. This repository
bumps to that release.

## Plan

The work lands in kixelated/mp4-atom. Validate the table against the output
channel count on decode rather than trusting it, and keep family 0 encoding
byte-identical. The bump here is a separate, small step once the release
ships.
