# [M] Bound the live archive reader index by retention

## Goal

A rolling archive reader tracks retained objects, and a seek across the expired boundary can reach the oldest retained group.

## Plan

The recorder prunes objects while the consumer index lists forward and forgets old keys after failed reads. The audit traced growth without rewinds and a seek that can stop on the first deleted object. Exercise an attached reader over repeated retention rotations and an evicted byte cache. Measure index size against retained objects, exercise a seek across the boundary, and fix any confirmed failure. Correct the archive guide's claim about time lookup.
