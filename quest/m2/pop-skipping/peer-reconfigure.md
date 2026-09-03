# [M] Peer reconfigure

## Goal

Reconfigure structured peer policy when negotiated versions or directional
costs change. Extend [moq#2874](https://github.com/moq-dev/moq/pull/2874) and
prove both directions of an asymmetric link without regressing the landed
cost, credential, or no-op behavior.
