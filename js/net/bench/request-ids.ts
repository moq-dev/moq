// Request-ID admission cost across independent sessions and reordered windows.
import { ControlStreamAdapter, SpentIds } from "../src/ietf/adapter.ts";
import { Version } from "../src/ietf/version.ts";
import { Stream } from "../src/stream.ts";

/**
 * Every ID in the window: in order, reversed (one skip to the top, then each fill
 * shortens it), or scattered (the top first, then every other ID descending, so each
 * splits the lowest skipped run and the remembered runs grow to half the window).
 */
function order(mode: string, window: number): bigint[] {
	const all = Array.from({ length: window }, (_, id) => BigInt(id * 2));
	if (mode === "ordered") return all;
	if (mode === "reordered") return all.reverse();
	const top = all[window - 1];
	const odd = all.filter((id, index) => index % 2 === 1 && id !== top).reverse();
	const even = all.filter((id, index) => index % 2 === 0 && id !== top).reverse();
	return [top, ...odd, ...even];
}

const debug = console.debug;
console.debug = () => undefined;
try {
	console.log("mode,sessions,window,admit_us,accepted");
	// Up to the production window, REQUEST_WINDOW.
	for (const mode of ["ordered", "reordered", "scattered"]) {
		for (const sessions of [1, 8, 32]) {
			for (const window of [8, 128, 220_000]) {
				const ids = order(mode, window);
				// Fewer rounds for the large window, keeping each sweep to a similar budget.
				const rounds = Math.max(1, Math.floor(8192 / window));
				let elapsed = 0;
				let accepted = 0;
				for (let round = 0; round < rounds; round++) {
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
					`${mode},${sessions},${window},${((elapsed * 1000) / rounds).toFixed(2)},${accepted / rounds}`,
				);
			}
		}
	}
} finally {
	console.debug = debug;
}

// Draft 17+ has no MAX_REQUEST_ID window, yet still remembers skipped runs to refuse reuse.
console.log("modern_mode,sessions,requests,admit_us,accepted");
for (const mode of ["ordered", "reordered", "scattered"]) {
	for (const sessions of [1, 8, 32]) {
		for (const requests of [8, 128, 220_000]) {
			const ids = order(mode, requests);
			const rounds = Math.max(1, Math.floor(8192 / requests));
			let elapsed = 0;
			let accepted = 0;
			for (let round = 0; round < rounds; round++) {
				const receivers = Array.from({ length: sessions }, () => new SpentIds(0n));
				const start = performance.now();
				for (const receiver of receivers) {
					for (const id of ids) {
						receiver.spend(id);
						accepted++;
					}
				}
				elapsed += performance.now() - start;
			}
			console.log(
				`modern-${mode},${sessions},${requests},${((elapsed * 1000) / rounds).toFixed(2)},${accepted / rounds}`,
			);
		}
	}
}
