/**
 * A JavaScript publisher that drives a native receiver into receive-credit exhaustion.
 *
 * Dials the Rust test's WebTransport server through the `@moq/web-transport` polyfill,
 * pinned to its certificate, and announces broadcast `room` with tracks `low` and `high`.
 * Each stdin line asks for one group, so the receiver decides when each one is written:
 *
 * - `low`: a frame twice the window on `low`, which can never complete.
 * - `high`: half a window on `high`, which only fits once the receiver stops `low`.
 * - `after`: a small group on `low`, which has to arrive after that stream is stopped.
 *
 *     bun sender.ts https://127.0.0.1:PORT/ VERSION CERT.pem WINDOW
 *
 * Exits 0 once stdin closes.
 *
 * @module
 */
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

// stdout is unused, but keep library debug logs off it all the same.
console.debug = console.error;

// Bun has no native WebTransport; @moq/net reads the global at connect time.
install();

const [address, version, certificate, windowArg] = process.argv.slice(2);
const window = Number(windowArg);
if (!address || !version || !certificate || !Number.isInteger(window) || window <= 0) {
	console.error("usage: sender.ts URL VERSION CERT.pem WINDOW");
	process.exit(2);
}

const origin = new Moq.Origin.Producer();
const broadcast = origin.createBroadcast(Moq.Path.from("room"));
const low = broadcast.createTrack("low");
const high = broadcast.createTrack("high");
broadcast.announce();

const session = await Moq.Connection.connect({
	url: new URL(address),
	publish: origin.consume(),
	versions: [version],
	webtransport: { serverCertificate: await Bun.file(certificate).text() },
	// The point is the native QUIC receiver; a WebSocket fallback would skip it.
	websocket: { enabled: false },
});

// Written once and left open: the receiver has to give up on it.
let partial: Moq.Group.Producer | undefined;

try {
	for await (const line of console) {
		const command = line.trim();
		// Bun yields a trailing empty line at EOF.
		if (command === "") continue;
		if (command === "low") {
			partial = low.appendGroup();
			partial.writeFrame({ payload: new Uint8Array(2 * window).fill(1) });
		} else if (command === "high") {
			const group = high.appendGroup();
			group.writeFrame({ payload: new Uint8Array(window / 2).fill(2) });
			group.close();
		} else if (command === "after") {
			const group = low.appendGroup();
			group.writeFrame({ payload: new TextEncoder().encode("after") });
			group.close();
		} else {
			throw new Error(`unknown command: ${command}`);
		}
	}
} finally {
	// End the live tracks, or the close waits out its deadline for them.
	partial?.close();
	low.close();
	high.close();
	await session.close();
	origin.close();
}
