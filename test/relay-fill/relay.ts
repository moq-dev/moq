// A JavaScript relay between Rust WebSocket peers, for `rs/moq-tokio/tests/integration/relay_fill.rs`:
// it advertises BROADCAST downstream and serves each request for it from the upstream session,
// whose received copy every downstream session shares, so a reader there misses whatever that
// copy lacks. Commands on stdin, each answered on stdout once done: `dial` opens another
// downstream session, so each reader has a copy of its own in front of this one, `evict` ages
// out every group the copy received but the live edge, on the relay's manual clock, and
// `fetches` reports how many downstream FETCHes the relay forwarded upstream.
import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { Manual } from "@moq/clock/test";
import { Session } from "@moq/qmux";
import { DEFAULT_EXPIRY } from "../../js/net/src/cache.ts";
import { connect } from "../../js/net/src/connection/connect.ts";
import { Producer } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";
import { wireOf } from "../../js/net/src/wire.ts";

// stdout carries the handshake to the Rust test, so library debug logs go to stderr. The FETCHes
// the relay's publisher serves are counted from them: each answers a downstream copy's FETCH by
// forwarding it upstream, past this relay's copy.
let forwarded = 0;
const counting =
	(log: (...args: unknown[]) => void) =>
	(...args: unknown[]) => {
		if (typeof args[0] === "string" && /^fetch (done|error):/.test(args[0])) forwarded++;
		log(...args);
	};
console.debug = counting(console.error);
console.warn = counting(console.warn);
console.info = console.error;

const [upstream, downstream, protocol, name] = process.argv.slice(2);
assert(upstream && downstream && protocol && name, "usage: relay.ts UPSTREAM_URL DOWNSTREAM_URL ALPN BROADCAST");
const path = Path.from(name);

// Installed before any session, so the copy's cache ages by it alone.
const clock = new Manual();
const installed = clock.install();
// A session never announces a received broadcast to another, so the relay advertises it itself.
const origin = new Producer();
const claim = origin.claim(path);

async function dial(address: string, publish?: ReturnType<Producer["consume"]>) {
	const url = new URL(address);
	// QMux-00 has no idle timeout, which the manual clock's jump would otherwise trip.
	const transport = new Session(url, {
		protocols: [protocol],
		versions: { [protocol]: "qmux-00" },
		requireProtocol: true,
	});
	await transport.ready;
	return connect({ url, transport, publish });
}

const source = await dial(upstream);
const sinks: Awaited<ReturnType<typeof dial>>[] = [];
const serving = (async () => {
	for await (const request of claim.requested()) request.serve(wireOf(source).consume(path));
})();
console.log("ready");
const lines = createInterface({ input: process.stdin });
try {
	for await (const line of lines) {
		if (line === "dial") {
			sinks.push(await dial(downstream, origin.consume()));
			console.log("dialed");
			continue;
		}
		if (line === "fetches") {
			console.log(`fetches ${forwarded}`);
			continue;
		}
		assert.equal(line, "evict", `unknown command: ${line}`);
		// Past the cache window and the sweep that trails it by up to half a window, so the
		// copy has evicted what nobody touched.
		await clock.advance(DEFAULT_EXPIRY.mul(2));
		console.log("evicted");
	}
} finally {
	lines.close();
	claim.close();
	await serving;
	for (const sink of sinks) await sink.close();
	await source.close();
	origin.close();
	installed.close();
}
