# [M] Guided setup

## Goal

The first launch of `moq-studio` gets a creator live without reading
settings: it detects cameras, microphones, and displays, measures the uplink,
probes which encoders work on this machine, and proposes a scene and output
settings in plain language. Every setting it chooses can be changed later and
explains itself.

## Plan

Twitch Studio did this before it was shut down, and "settings are too
complicated" is a top OBS complaint. Reuse the probes that already exist
(the encoder choice `moq_video::render::export` makes at setup, the bandwidth
grant) rather than a separate benchmark. A probe that fails is reported, not
hidden.
