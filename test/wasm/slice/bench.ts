/**
 * Per-call costs of the moq-ffi slice in the browser, identical for each generator.
 *
 * `driver.ts` opens a fresh page per round and passes which generator's adapter to load.
 *
 * @module
 */
import type { Open } from "./adapter.ts";

export interface Bench {
	name: string;
	iterations: number;
	us_per_op: number;
}

export interface Report {
	generator: string;
	init_ms: number;
	bench: Bench[];
}

// Each batch is a fresh group, under moq-net's 8192-frame and 32 MiB group caps.
const BATCH = 1000;

async function run(generator: string): Promise<Report> {
	const start = performance.now();
	// Runtime URLs keep each generator's bundle out of the other's page.
	const { open } = (await import(`/slice-${generator}.js`)) as { open: Open };
	const slice = await open(new URL(`/slice/${generator}.wasm`, location.href));
	const init_ms = performance.now() - start;

	const bench: Bench[] = [];
	const measure = async (name: string, batches: number, per: number, body: () => Promise<number>) => {
		await body(); // warm the dispatchers
		let elapsed = 0;
		for (let i = 0; i < batches; i++) elapsed += await body();
		bench.push({ name, iterations: batches * per, us_per_op: (elapsed * 1000) / (batches * per) });
	};
	const timed = async (body: () => unknown | Promise<unknown>) => {
		const start = performance.now();
		for (let i = 0; i < BATCH; i++) await body();
		return performance.now() - start;
	};

	await measure("sync noop()", 10, BATCH, async () => {
		const start = performance.now();
		for (let i = 0; i < BATCH; i++) slice.noop();
		return performance.now() - start;
	});
	await measure("sync getter: sequence()", 10, BATCH, async () => {
		const group = slice.group();
		const start = performance.now();
		for (let i = 0; i < BATCH; i++) group.sequence();
		const elapsed = performance.now() - start;
		group.close();
		return elapsed;
	});
	await measure("async noop_async()", 5, BATCH, () => timed(() => slice.noopAsync()));
	await measure("async yield_once(): one wakeup", 5, BATCH, () => timed(() => slice.yieldOnce()));

	for (const size of [160, 4096, 131072]) {
		const bytes = new Uint8Array(size).map((_, i) => i % 251);
		const per = Math.min(BATCH, (16 << 20) / size);
		const batches = size > 4096 ? 8 : 10;
		await measure(`sync write_frame ${size}B`, batches, per, async () => {
			const group = slice.group();
			const start = performance.now();
			for (let i = 0; i < per; i++) group.write(bytes);
			const elapsed = performance.now() - start;
			group.close();
			return elapsed;
		});
		await measure(`async read_frame, buffered ${size}B`, batches, per, async () => {
			const group = slice.group();
			for (let i = 0; i < per; i++) group.write(bytes);
			const start = performance.now();
			for (let i = 0; i < per; i++)
				if ((await group.read())?.payload.byteLength !== size) throw new Error("wrong frame size");
			const elapsed = performance.now() - start;
			group.close();
			return elapsed;
		});
	}

	const bytes = new Uint8Array(160);
	await measure("async read_frame, parked then woken 160B", 5, BATCH, async () => {
		const group = slice.group();
		const start = performance.now();
		for (let i = 0; i < BATCH; i++) {
			const read = group.read();
			group.write(bytes);
			await read;
		}
		const elapsed = performance.now() - start;
		group.close();
		return elapsed;
	});
	return { generator, init_ms, bench };
}

declare global {
	interface Window {
		sliceBench: typeof run;
	}
}
window.sliceBench = run;
