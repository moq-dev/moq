---
title: Troubleshooting
description: Find the right diagnostics for connection, discovery, playback, and build problems
---

# Troubleshooting

Start at the layer where progress stops. [Inspect a relay](/bin/inspect)
shows how to list broadcasts and read a catalog or cached group without a
player, which helps separate delivery from media playback.

| Symptom or question | Look here |
| --- | --- |
| The local demo does not start | [Quick start](/setup/), [development tools and Windows setup](/setup/dev) |
| A client cannot connect, or uses WebSocket | [Transport](/concept/transport), [browser support](/lib/js/#browser-support), [production TLS](/setup/prod) |
| Authentication fails or a path is inaccessible | [Authentication](/bin/relay/auth): URL scope, tokens, and permissions; [relay configuration](/bin/relay/config) |
| Connected, but a broadcast is missing | [Inspect a relay](/bin/inspect#what-is-live), then [discovery and subscriptions](/concept/moq-lite) |
| A broadcast exists, but there is no playable media | [Fetch its catalog](/bin/inspect#what-is-in-a-group), [hang media model](/concept/hang), [player](/lib/js/watch) and [publisher](/lib/js/publish) guides |
| Audio gaps, stalls, or growing latency | [Audio jitter](/concept/audio-jitter), [subscription behavior](/concept/moq-lite), [stats](/concept/stats) |
| An existing RTMP, SRT, WebRTC, or HLS tool cannot exchange media | The corresponding [gateway guide](/bin/#gateways) and its CLI `--help` |
| Traffic takes the wrong relay route, or failover behaves unexpectedly | [Clustering](/bin/relay/cluster), [redundant ingest](/setup/redundancy) |
| An example uses an API missing from the installed package | [Upgrade](/setup/upgrade), [source and package versions](/setup/repository#read-the-right-version) |
| A repository check fails | [Development](/setup/dev#checks-and-tests), [test harness logs](https://github.com/moq-dev/moq/blob/main/test/README.md#keeping-a-failure) |

## Investigate or report a problem

For local debugging, [Development](/setup/dev#debugging) covers Rust logs and
backtraces. The browser console and the component support elements described
under [browser support](/lib/js/#browser-support) help identify transport and
codec failures.

When sharing a reproduction, include the package versions or checkout commit,
OS and browser, the command or smallest example, what you expected, and the
relevant error. Remove JWTs, private URLs, and credentials from logs. Use
[GitHub issues](https://github.com/moq-dev/moq/issues) for a reproducible bug
or [Discord](https://discord.moq.dev) for help finding the right starting point.
