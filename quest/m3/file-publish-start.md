# [S] Pace file publication from the recording's first media time

## Goal

A browser file source starts promptly and loops after the elapsed media duration even when its initial timestamps are nonzero.

## Plan

A one-second MPEG-TS with initial PTS 120 seconds scheduled its first frame at 120 seconds and its next pass at 241 seconds in the demuxer and file pacing code. Normalize audio and video to a shared origin, preserve their relative offset, and check the first two passes with a simulated clock.
