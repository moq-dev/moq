import { expect, test } from "bun:test";
import { createServer } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch } from "./harness";

test("encoded preview retains native codecs across bitrate changes and recovers only on config", async () => {
	const server = await createServer({
		configFile: false,
		root: `${import.meta.dir}/encoded-preview`,
		plugins: [worklet()],
		server: { host: "127.0.0.1", port: 0 },
	});
	await server.listen();
	const browser = await launch();
	try {
		const page = await browser.newPage();
		const errors: string[] = [];
		page.on("pageerror", (e) => errors.push(e.message));
		await page.goto(server.resolvedUrls?.local[0] ?? "");
		await page.evaluate(async () => {
			await import(`${location.origin}/main.ts`);
		});
		const first = await page.evaluate(async () => {
			const p = await import(`${location.origin}/main.ts`);
			return p.emit();
		});
		expect(first).toMatchObject({ encoders: 1, decoders: 1, frame: true, width: 64 });
		for (const bitrate of [500000, 2000000, 750000]) {
			const changed = await page.evaluate(async (rate) => {
				const p = await import(`${location.origin}/main.ts`);
				return p.bitrate(rate);
			}, bitrate);
			expect(changed).toMatchObject({ encoders: 1, decoders: 1, frame: true, retained: true });
			const next = await page.evaluate(async () => {
				const p = await import(`${location.origin}/main.ts`);
				return p.emit();
			});
			expect(next.timestamp).toBeGreaterThan(first.timestamp);
		}
		for (const [index, kind] of (["encode", "decode"] as const).entries()) {
			const failed = await page.evaluate(async (kind) => {
				const p = await import(`${location.origin}/main.ts`);
				return p.fail(kind);
			}, kind);
			expect(failed).toMatchObject({ encoders: index + 1, decoders: index + 1, frame: false });
			await page.evaluate(async (rate) => {
				const p = await import(`${location.origin}/main.ts`);
				return p.bitrate(rate);
			}, 1000000 + index);
			const recovered = await page.evaluate(async () => {
				const p = await import(`${location.origin}/main.ts`);
				return p.emit();
			});
			expect(recovered).toMatchObject({ encoders: index + 2, decoders: index + 2, frame: true });
		}
		await page.evaluate(async () => {
			const p = await import(`${location.origin}/main.ts`);
			return p.resize();
		});
		const resized = await page.evaluate(async () => {
			const p = await import(`${location.origin}/main.ts`);
			return p.emit();
		});
		expect(resized).toMatchObject({ encoders: 4, decoders: 4, frame: true, width: 96 });
		const closed = await page.evaluate(async () => {
			const p = await import(`${location.origin}/main.ts`);
			return p.close();
		});
		expect(closed.closed).toBe(true);
		expect(closed.state.frame).toBe(false);
		expect(errors).toEqual([]);
	} finally {
		await browser.close();
		await server.close();
	}
}, 30000);
