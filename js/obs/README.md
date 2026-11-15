<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/obs

[![npm version](https://img.shields.io/npm/v/@moq/obs)](https://www.npmjs.com/package/@moq/obs)

Set up the OBS Studio on the user's own machine from a web page, through OBS's built-in
obs-websocket server (OBS 28 or newer), so a page that hands out a
[Media over QUIC](https://moq.dev/) relay URL configures OBS in one click.

```ts
import * as Obs from "@moq/obs";

// The password from OBS's Tools > WebSocket Server Settings. It stays in memory.
const obs = await Obs.connect({ password });
console.log(obs.version.obs, obs.plugin); // "32.2.2", true when the MoQ plugin is loaded

// A dedicated "MoQ" profile, tuned for low latency, with three simulcast renditions.
const { ignored } = await obs.configure({
	destination: { kind: "moq", url: "https://relay.example.com/anon", broadcast: "me.hang", layers: 3 },
	video: { size: { width: 1920, height: 1080 }, fps: 30, bitrate: 6000 },
});
if (ignored.length > 0) console.warn("this encoder keeps its own", ignored);

await obs.start();
obs.stats.subscribe((stats) => console.log(stats?.bitrate, stats?.frames.dropped));
await obs.stop();

// Play a broadcast in the scene on program.
await obs.addSource({ url: "https://relay.example.com/anon", broadcast: "guest.hang" });
obs.close();
```

`connect` fails with a `ConnectError` whose `reason` tells the page what to say:

- `denied`: the browser was not allowed to reach apps on this device. Chrome 147+ and Firefox ask
  first when a public page opens a WebSocket to loopback.
- `unreachable`: OBS is not running, or its WebSocket server is off.
- `refused`: the password is wrong, or OBS needs one.
- `blocked` or `unsupported`: the browser refused the socket, or the server is not obs-websocket 5.

Safari cannot reach OBS from an HTTPS page: it blocks `ws://` to loopback as mixed content.

`configure` refuses while any OBS output runs, and keeps the user's other profiles as they were.
An RTMP(S) destination (`{ kind: "rtmp", server, key }`) works with a stock OBS; `moq` destinations
and `addSource` need the [MoQ plugin](https://doc.moq.dev/bin/obs).
Whoever holds the password controls that OBS, so keep it only as long as the page needs it.
