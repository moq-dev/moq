# [M] Multiple canvases

## Goal

The studio composites several canvases at once from shared sources, such as
a 1920x1080 landscape canvas and a 1080x1920 portrait one. Each canvas has
its own layout per scene, its own preview, and its own outputs. A source
captures or decodes once however many canvases use it.

## Plan

Each canvas publishes its own broadcast, because different content needs a
different name. Transitions are per canvas, so a scene change cuts every
canvas together unless the creator unlinks one.

Benchmark source count against canvas count, including sparse reuse. Per-canvas
cost must grow only with the sources that canvas uses, never with the whole
source table.
