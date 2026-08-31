# [XL] Native and Mobile Support

## Goal

Implement and verify the behavior tracked in [#700](https://github.com/moq-dev/moq/issues/700)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

It's great that MoQ works on browsers. However, by primarily targeting the web, we make it harder to run MoQ natively.

Obviously, we could get the Rust code working on native platforms but it lacks a lot of functionality that only exists in the JS code. We would also need a replacement for WebCodecs and rendering.

Alternatively, we could figure out how to get something like React Native running using the JS code? We would need WebTransport and WebCodecs support of course so it might be the same issue.

## Closes

- [#700](https://github.com/moq-dev/moq/issues/700) - close this issue when the quest finishes
