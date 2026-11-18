# [M] Publish only JSON records the companion reader accepts

## Goal

Every record a JS stream, snapshot or window successfully publishes can be decoded under the package's JSON wire rules.

## Plan

The JS stream encoder accepted and committed half of a surrogate pair and a 128-level nested array. Its decoder rejected both. [moq-dev/moq#6863](https://github.com/moq-dev/moq/pull/6863) introduced Unicode rejection and [moq-dev/moq#8320](https://github.com/moq-dev/moq/pull/8320) added nesting validation. Validate the serialized shape including the window envelope, before emitting a frame or committing encoder state. Exercise each public writer followed by its reader, including a valid write after rejection.
