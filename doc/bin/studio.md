---
title: Studio
description: Compose scenes on the GPU and go live over MoQ from a native app
---

# Studio

`moq-studio` is a native streaming studio for Linux, macOS, and Windows. It
composes cameras, screens, windows, images, colors, and broadcasts from a relay
into scenes on the GPU, mixes their audio, and publishes the result to a MoQ
relay.

It lives in its own repository, [moq-dev/studio](https://github.com/moq-dev/studio),
whose README covers building it, its panels, the scene file, and its output.
It builds on this repository's [moq-video](/lib/rs/moq-video) and
[moq-audio](/lib/rs/moq-audio).
