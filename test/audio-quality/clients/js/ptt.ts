/** Run 22 microphone presses through the real publisher, relay, and player. @module */
import { join } from "node:path";
import { parseArgs } from "node:util";
import { createServer } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch } from "../../../interop/clients/js/harness";
import type { measurement } from "./src/ptt";

const { values } = parseArgs({
	options: {
		publisher: { type: "string" },
		viewer: { type: "string" },
		out: { type: "string" },
	},
});
const { publisher, viewer, out } = values;
if (!publisher || !viewer || !out) throw new Error("usage: ptt.ts --publisher URL --viewer URL --out DIR");
const hash = await fetch(`${publisher}/certificate.sha256`).then(async (r) => {
	if (!r.ok) throw new Error(`certificate: ${r.status}`);
	return r.text();
});

// Chromium loops this constant tone as its microphone. Its default fake device has silent
// intervals of its own, which would make an audible onset ambiguous.
const wav = Buffer.alloc(44 + 48_000 * 2);
wav.write("RIFF", 0);
wav.writeUInt32LE(wav.length - 8, 4);
wav.write("WAVEfmt ", 8);
wav.writeUInt32LE(16, 16);
wav.writeUInt16LE(1, 20);
wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(48_000, 24);
wav.writeUInt32LE(96_000, 28);
wav.writeUInt16LE(2, 32);
wav.writeUInt16LE(16, 34);
wav.write("data", 36);
wav.writeUInt32LE(wav.length - 44, 40);
for (let i = 0; i < 48_000; i++)
	wav.writeInt16LE(Math.round(8192 * Math.sin((2 * Math.PI * 440 * i) / 48_000)), 44 + i * 2);
const tone = join(out, "tone.wav");
await Bun.write(tone, wav);

const server = await createServer({
	configFile: false,
	root: import.meta.dir,
	plugins: [worklet()],
	server: { host: "127.0.0.1", port: 0 },
});
await server.listen();
const browser = await launch([
	"--autoplay-policy=no-user-gesture-required",
	"--use-fake-device-for-media-stream",
	"--use-fake-ui-for-media-stream",
	`--use-file-for-fake-audio-capture=${tone}`,
]);
try {
	const page = await browser.newPage();
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(String(error)));
	await page.route(`${viewer}/certificate.sha256`, (route) =>
		route.fulfill({ body: hash, headers: { "access-control-allow-origin": "*" } }),
	);
	await page.goto(`${server.resolvedUrls?.local[0]}ptt.html?${new URLSearchParams({ publisher, viewer })}`);
	await page.waitForFunction(() => globalThis.ptt?.state().selected && globalThis.ptt.state().tapped, undefined, {
		timeout: 30_000,
	});
	const initial = await page.evaluate(() => globalThis.ptt.state());
	if (initial.publisher !== "webtransport" || initial.viewer !== "webtransport") throw new Error("not WebTransport");
	await page.waitForTimeout(3000);
	await page.evaluate(() => globalThis.ptt.press(false));
	await page.waitForTimeout(5000);
	const held = await page.evaluate(() => globalThis.ptt.state());
	if (held.active || held.config?.enabled !== false || held.selected !== initial.selected) {
		throw new Error("the muted listener did not hold its rendition");
	}
	const warmup = await page.evaluate(() => globalThis.ptt.result());
	await Bun.write(join(out, "warmup.json"), JSON.stringify({ browser: browser.version(), initial, held, ...warmup }));
	if (!warmup.audible.length) throw new Error("the tone never reached the output tap");
	await page.evaluate(() => globalThis.ptt.reset());
	for (let i = 0; i < 22; i++) {
		await page.waitForTimeout(i < 8 ? 0 : i < 15 ? 5000 : 60_000);
		await page.evaluate(() => globalThis.ptt.press(true));
		await page.waitForTimeout(i % 2 === 0 ? 2000 : 3000);
		await page.evaluate(() => globalThis.ptt.press(false));
		console.log(`press ${i + 1}/22`);
		await Bun.write(join(out, "presses.json"), JSON.stringify(await page.evaluate(() => globalThis.ptt.result())));
	}
	await page.waitForTimeout(5000);
	const result: ReturnType<typeof measurement.result> = await page.evaluate(() => globalThis.ptt.result());
	const state = await page.evaluate(() => globalThis.ptt.state());
	await Bun.write(
		join(out, "presses.json"),
		JSON.stringify({ browser: browser.version(), ...result, state, errors }),
	);
	if (errors.length || result.errors.length || result.captureError || state.error)
		throw new Error("measurement failed; see presses.json");
} finally {
	await browser.close();
	await server.close();
}
