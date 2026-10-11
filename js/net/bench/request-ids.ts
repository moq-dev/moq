// Request-ID admission cost across independent sessions and reordered windows.
import { ControlStreamAdapter } from "../src/ietf/adapter.ts";
import { Version } from "../src/ietf/version.ts";
import { Stream } from "../src/stream.ts";

const debug = console.debug;
console.debug = () => undefined;
try {
	console.log("mode,sessions,window,admit_us,accepted");
	for (const reversed of [false, true]) {
		for (const sessions of [1, 8, 32]) {
			for (const window of [8, 32, 128]) {
				const ids = Array.from({ length: window }, (_, id) => BigInt(id * 2));
				if (reversed) ids.reverse();
				let elapsed = 0;
				let accepted = 0;
				for (let round = 0; round < 64; round++) {
					const fixtures = Array.from({ length: sessions }, () => {
						const stream = new Stream({
							readable: new ReadableStream(),
							writable: new WritableStream(),
							version: Version.DRAFT_16,
						});
						const quic = { close() {} } as unknown as WebTransport;
						return {
							stream,
							adapter: new ControlStreamAdapter(
								quic,
								stream,
								Version.DRAFT_16,
								0n,
								false,
								BigInt(window),
							),
						};
					});
					const start = performance.now();
					for (const { adapter } of fixtures) {
						for (const id of ids) {
							try {
								adapter.admitRequest(id);
								accepted++;
							} catch {
								/* Count refusals in baseline comparisons. */
							}
						}
					}
					elapsed += performance.now() - start;
					for (const { stream, adapter } of fixtures) {
						adapter.close();
						stream.close();
					}
					await Promise.resolve();
				}
				console.log(
					`${reversed ? "reordered" : "ordered"},${sessions},${window},${((elapsed * 1000) / 64).toFixed(2)},${accepted}`,
				);
			}
		}
	}
} finally {
	console.debug = debug;
}
