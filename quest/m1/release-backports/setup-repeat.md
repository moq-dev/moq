# [XS] Repeated unknown SETUP options on release

## Goal

On `release`, a moq-transport peer that repeats an unknown or GREASE SETUP
option completes the handshake instead of failing with `Duplicate`.

## Plan

Backport only the SETUP half of #4927 (`4b53bd27d`): unknown option kinds may
repeat, known ones still refuse duplicates. Skip #4927's stricter GROUP_ORDER
validation, which tightens behavior rather than fixing a break.
