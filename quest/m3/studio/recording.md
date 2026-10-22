# [M] Studio recording

## Goal

The studio records each canvas's program to disk, and optionally each source
as its own isolated track, independent of whether it is live. Recording
starts and stops on its own without touching live outputs.

## Plan

Write fMP4 through moq-mux. A recording whose encode settings match a live
destination shares that destination's encoder.

## Required

- [The studio app](/quest/m3/studio/app.md) - the program and encoders a recording reuses
