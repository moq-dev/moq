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

// Modern streams have no MAX_REQUEST_ID window. Sweep history fragmentation separately
// from session fanout, including reverse insertion into a fragmented history.
const { RequestIds } = await import("../src/ietf/request.ts");
// The old modern path admitted every ID without tracking it.
const baseline = process.argv.includes("--baseline");
console.log("modern_mode,sessions,ranges,admit_us,accepted,duplicates_refused");
for (const fragmented of [false, true]) {
	for (const sessions of [1, 8, 32]) {
		for (const ranges of [8, 32, 128]) {
			const ids = Array.from({ length: ranges }, (_, i) => BigInt(i * (fragmented ? 4 : 2)));
			if (fragmented) ids.reverse();
			let elapsed = 0;
			let accepted = 0;
			let duplicates = 0;
			for (let round = 0; round < 64; round++) {
				const receivers = Array.from({ length: sessions }, () =>
					baseline ? { accept(_id: bigint) {} } : new RequestIds(false),
				);
				const start = performance.now();
				for (const receiver of receivers)
					for (const id of ids) {
						receiver.accept(id);
						accepted++;
					}
				elapsed += performance.now() - start;
				for (const receiver of receivers) {
					try {
						receiver.accept(ids[0] ?? 0n);
					} catch {
						duplicates++;
					}
				}
			}
			console.log(
				`modern-${fragmented ? "fragmented" : "contiguous"},${sessions},${ranges},${((elapsed * 1000) / 64).toFixed(2)},${accepted},${duplicates}`,
			);
		}
	}
}
