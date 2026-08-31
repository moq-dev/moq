# [M] Default to enabled: true

## Goal

Implement and verify the behavior tracked in [#1238](https://github.com/moq-dev/moq/issues/1238)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Standalone classes across `@moq/net`, `@moq/watch`, and `@moq/publish` accept an optional reactive `enabled` input, but most default it to `false`. Omitting the property therefore constructs a module that silently does nothing.

The public contract should make the direct constructor useful by default while preserving explicit and reactive lifecycle control.

#### Decision

Keep the positive `enabled` input. Do not rename it to `disabled`.

For standalone JavaScript components:

- an omitted or `undefined` `enabled` input defaults to `true`;
- explicit `false` keeps the component inactive;
- changing a live input from `true` to `false` releases active resources;
- changing it back to `true` restarts normally; and
- this is the general convention for future standalone components unless an API explicitly documents an exception.

Apply the change atomically to the current default-disabled public classes on `dev`:

- `@moq/net`: `Connection.Reload`
- `@moq/watch`: `Broadcast`, `Video.Decoder`, `Audio.Decoder`, `Text.Renderer`
- `@moq/publish`: `Broadcast`, `Video.Encoder`, `Audio.Capture`, `Audio.Encoder`, `Source.Camera`, `Source.Microphone`, `Source.Screen`, `Source.File`

Direct construction of camera, microphone, or screen capture authorizes the component to request capture immediately. Screen capture still has the browser's user-gesture requirement, which the public documentation must state.

The `<moq-watch>` and `<moq-publish>` custom elements keep their existing behavior because they already supply explicit lifecycle-controlled signals. Connection pooling and publish preview already default enabled and remain unchanged.

#### Acceptance criteria

- \[ ] Each listed class treats omitted and explicit `undefined` `enabled` as `true`.
- \[ ] Explicit `enabled: false` prevents connection, subscription, encoding, rendering, and permission requests.
- \[ ] A live `enabled` input still tears down resources on `true -> false` and restarts on `false -> true`.
- \[ ] High-level custom-element lifecycle behavior is unchanged.
- \[ ] Redundant static `enabled: true` examples are removed; reactive and explicit-false examples remain where they communicate lifecycle policy.
- \[ ] Public docs describe the default and call out the screen-capture user-gesture constraint.
- \[ ] The change lands as one breaking PR targeting `dev`.

#### Non-goals

- Renaming `enabled` to `disabled` or adding a compatibility alias.
- Redesigning capture error reporting, retry policy, or dismissal handling.
- Changing custom-element activation behavior.
- Changing connection-pool or publish-preview behavior.

## Closes

- [#1238](https://github.com/moq-dev/moq/issues/1238) - close this issue when the quest finishes
