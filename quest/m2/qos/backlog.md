# [M] MoQ backlog counters

## Goal

Moq-net reports per-subscription queued-versus-delivered bytes and groups plus
skip counters through the released backend-neutral transport hook.

## Plan

Implement [#2733](https://github.com/moq-dev/moq/issues/2733) after the
transport hook is released. Map stream counters into per-subscription send
backlog and skip counters without exposing a backend type or aggregating
across projects. Cover queue growth, delivery, skip, reset, and teardown.

The moq.pro (downstream) health views consume the released counters
downstream.

## Required

- A moq-dev/web-transport release carries the backend-neutral per-stream delivered-vs-queued hook with a Quinn implementation (moq-dev/web-transport#368)

## Closes

- [#2733](https://github.com/moq-dev/moq/issues/2733) - close this issue when the quest finishes
