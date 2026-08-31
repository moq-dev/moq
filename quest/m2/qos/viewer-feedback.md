# [L] Viewer feedback

## Goal

Sampled per-viewer QoS reaches monitoring consumers, so health reflects what
viewers actually experienced rather than what the relay believes it sent.

## Plan

Implement [#2735](https://github.com/moq-dev/moq/issues/2735): opt-in viewer
feedback broadcasts with per-viewer sampling. Deliberately the last phase; it
is the only half that costs viewer bandwidth and carries a privacy surface, so
it should land after a health verdict built on the other halves proves what is
actually missing.

No PR exists yet, so writing the implementation is this quest's own work.

Opt-in and sampled are both load-bearing: a feedback broadcast per viewer at
full rate is the fan-out problem again, pointed at ourselves.

## Required

- The moq.pro (downstream) health badge ships first by design, so the verdict exists before viewer bandwidth is spent refining it

## Closes

- [#2735](https://github.com/moq-dev/moq/issues/2735) - close this issue when the quest finishes
