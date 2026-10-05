---
title: Inspect a relay
description: See what is live on a relay, watch it change, read a group, and read its stats
---

# Inspect a relay

Every question here has two answers: a [`moq`](/bin/cli) verb that speaks MoQ
with the session's own auth, and a `curl` against the relay's
[HTTP endpoints](/bin/relay/http). The examples use the local dev relay from
`just dev`; in production, swap in your relay's URL and add a `?jwt=` token.

| Question | MoQ | HTTP |
| --- | --- | --- |
| What is live? | `moq --connect <url>/<path> ls [prefix]` | `GET /announced/<path>` |
| What changes? | `moq ... ls --follow` | (none) |
| What is in a group? | `moq ... --broadcast <name> fetch <track>` | `GET /fetch/<path>/<name>/<track>` |

## What is live

```bash
moq --connect http://localhost:4443/anon ls
curl http://localhost:4443/announced/anon
```

Both print one broadcast path per line, relative to the path in the URL, and
both use that path for auth, so a token rooted at `rooms/123` lists only its
room. `moq ls room` narrows the listing to one prefix, and `--json` prints
`{"path": "room/alice", "active": true}` per line instead.

Both list announced prefixes, which by convention are broadcast paths. A
segment starting with `.` stays hidden unless the URL path or `prefix` names
it, which keeps the relay's own `.stats` out of the listing.

## What changes

```bash
moq --connect http://localhost:4443/anon ls --follow
```

`--follow` prints `+ path` for each live broadcast, then `+ path` and `- path`
as broadcasts come and go, until you stop it. It exits non-zero if the session
ends, so a script can tell a lost relay from a quiet one. `curl` has no
equivalent: `/announced` is a snapshot.

## What is in a group

```bash
moq --connect http://localhost:4443/anon --broadcast demo/bbb.hang fetch catalog.json | jq
curl http://localhost:4443/fetch/anon/demo/bbb.hang/catalog.json | jq
```

Both write the frame payloads of one group back to back: the newest group, or
the one `--group 42` (`?group=42`) names. `moq fetch --json` prints one line
per frame instead, `{"group": 42, "frame": 0, "size": 1234, "payload": "<base64>"}`,
which shows where the frames split.

`fetch` takes the track name literally. `/fetch` splits its path on the last
`/`, so it cannot reach a track whose name contains one. Both give up after 30
seconds, and `moq fetch` exits non-zero when the broadcast or group is missing.

## Stats

A relay with [`[stats]`](/bin/relay/config#stats) enabled publishes its
counters as ordinary broadcasts under `.stats`, so the same tools read them.
Dial the `.stats` path, which the token must cover:

```bash
moq --connect http://localhost:4443/.stats ls
moq --connect http://localhost:4443/.stats --broadcast node/local/host fetch publisher.json | jq
curl http://localhost:4443/fetch/.stats/node/local/host/publisher.json | jq
```

The node name (`local/host` here) is `stats.node`. Each fetch returns the
newest snapshot: a JSON object of cumulative counters keyed by broadcast path.
`publisher.json` is egress, `subscriber.json` is ingress, and `sessions.json`
counts sessions per auth root. Fetch twice and divide by the interval for a
rate. The `.json.z` twins carry compressed patches, which these tools print as
raw bytes. [Stats](/concept/stats) describes every field.
