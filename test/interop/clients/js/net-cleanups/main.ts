/** Public net consumers and native browser streams, plus the shared fan-out sweep. @module */

import { Manual, Milli } from "@moq/clock";
import * as Net from "@moq/net";
import { fanout } from "../../../../../js/net/bench/compression-fanout";
import { createMockTransportPair } from "../../../../../js/net/src/mock";

/** Exercise public readers and accepted connections in the browser. */
export async function run() {
	const group = new Net.Group.Producer(0);
	group.writeFrame({ payload: new TextEncoder().encode("first") });
	const first = group.consume();
	const a = await first.readString();
	const second = group.consume();
	const b = await second.readString();
	first.close();
	group.writeFrame({ payload: new TextEncoder().encode("second") });
	const c = await second.readString();
	group.finish();
	second.close();
	const json = new Net.Group.Producer(0);
	const reader = json.consume();
	json.writeFrame({ payload: new Uint8Array() });
	json.writeFrame({ payload: new TextEncoder().encode("false") });
	json.finish();
	let malformed = false;
	try {
		await reader.readJson();
	} catch (error) {
		malformed = error instanceof SyntaxError;
	}
	const value = await reader.readJson();
	reader.close();
	const probes: { rate?: number; rtt?: number }[] = [];
	for (const alpn of ["moq-lite-03", "moq-lite-04"]) {
		const clock = new Manual();
		const installed = clock.install();
		const pair = createMockTransportPair(alpn, { stats: { estimatedSendRate: 8_000_000, smoothedRtt: 12 } });
		const controller = new AbortController();
		const url = new URL("https://example.com");
		const [client, server] = await Promise.all([
			Net.Connection.connect({ url, transport: pair.client }),
			Net.Connection.accept({ url, transport: pair.server, timeout: Milli(1_000), signal: controller.signal }),
		]);
		const dispose = client.probe.subscribe(() => {});
		try {
			controller.abort(); // Acceptance no longer owns the session's lifetime.
			// A shared write budget may yield a host task mid-delivery, so drive time until the estimate lands.
			await clock.drive(
				(async () => {
					while (client.probe.peek().estimatedRecvRate === undefined) await client.probe.changed();
				})(),
			);
			probes.push({ rate: client.probe.peek().estimatedRecvRate, rtt: client.probe.peek().rtt });
		} finally {
			dispose();
			client.abort();
			server.abort();
			installed.close();
		}
	}
	return { cursors: [a, b, c], malformed, value, probes, rows: fanout() };
}
