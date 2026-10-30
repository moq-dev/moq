# [XS] The carla demo pins its driver epoch

## Goal

The carla demo's driver watcher resolves each driver run pinned to the epoch it
was announced under, so a lease never reads a newer run's commands.

## Plan

Split from [Two broadcast resolutions](/quest/m1/api-audit/resolve/README.md)
on 2026-10-28 (maintainer). Every other first-party caller now resolves once
with `request` or `follow`; see
[moq-dev/moq#7192](https://github.com/moq-dev/moq/pull/7192).

`watch_drivers` in `demo/carla/sim/moq_carla_sim/publish.py` takes each
driver's epoch from its announcement, then `_pump_driver` requests the path
unpinned. A newer run that took over in between is read under the old run's
lease until its own announcement cancels the task. Pass the announced epoch
to the Python wrapper's `request(path, epoch=...)`.

Run the demo's sim tests (`demo/carla/sim`).

Public API: none. Wire: none.
