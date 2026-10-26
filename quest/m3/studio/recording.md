# [M] Studio recording

## Goal

The studio records each canvas's program to disk, and optionally each source
as its own isolated track, independent of whether it is live. Recording
starts and stops on its own without touching live outputs.

## Plan

Write fMP4 through moq-mux. A recording shares a live destination's encoder
only when both have the same input (the same canvas or source, and the same
audio mix) and identical encode settings. An isolated source track never
shares the program's encoder.
