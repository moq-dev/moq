// Plays a WAV as Chromium's fake microphone into page/, records both encodes, and writes
// <out>-voice.wav, <out>-auto.wav, and <out>.json (frames and settings).
// Usage: bun drive.ts <input.wav> <seconds> <out prefix> [processing on|off] [chromium|firefox]
import { writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { firefox } from "playwright";
import { createServer } from "vite";
import { worklet } from "../../js/common/vite-plugin-worklet";
import { launch, sleep } from "../../test/interop/clients/js/harness";

const [input, secondsArg, out, processing = "on", engine = "chromium"] = process.argv.slice(2);
const seconds = Number(secondsArg);
const root = resolve(import.meta.dir, "page");

const server = await createServer({
	configFile: false,
	root,
	plugins: [worklet()],
	server: { host: "127.0.0.1", port: 0, fs: { allow: [resolve(import.meta.dir, "../..")] } },
	logLevel: "warn",
});
await server.listen();

const browser =
	engine === "chromium"
		? await launch([
				"--autoplay-policy=no-user-gesture-required",
				"--use-fake-device-for-media-stream",
				"--use-fake-ui-for-media-stream",
				`--use-file-for-fake-audio-capture=${resolve(input)}`,
			])
		: await firefox.launch({
				headless: true,
				env: {
					...process.env,
					LD_LIBRARY_PATH: [process.env.PLAYWRIGHT_FIREFOX_LIBRARY_PATH, process.env.LD_LIBRARY_PATH]
						.filter(Boolean)
						.join(":"),
				},
				firefoxUserPrefs: {
					"media.navigator.permission.disabled": true,
					"media.navigator.streams.fake": false,
					"media.autoplay.default": 0,
				},
			});
try {
	const context = await browser.newContext(engine === "chromium" ? { permissions: ["microphone"] } : {});
	const page = await context.newPage();
	page.on("pageerror", (e) => console.error("[page]", e.message));
	page.on("console", (m) => {
		if (m.type() === "error" || m.type() === "warning") console.error(`[${m.type()}]`, m.text());
	});
	await page.goto(`${server.resolvedUrls?.local[0]}index.html?processing=${processing}`);
	await page.waitForFunction(() => globalThis.vad && globalThis.vad.counts().auto > 0, undefined, { timeout: 30_000 });
	console.log("settings", JSON.stringify(await page.evaluate(() => globalThis.vad.settings())));
	await sleep(seconds * 1000 + 500);
	const result = await page.evaluate((s) => globalThis.vad.result(s), seconds);
	for (const kind of ["voice", "auto", "input"]) {
		const pcm = Buffer.from(result[kind].pcm, "base64");
		const header = Buffer.alloc(44);
		header.write("RIFF", 0);
		header.writeUInt32LE(36 + pcm.length, 4);
		header.write("WAVEfmt ", 8);
		header.writeUInt32LE(16, 16);
		header.writeUInt16LE(3, 20); // float
		header.writeUInt16LE(1, 22);
		header.writeUInt32LE(48_000, 24);
		header.writeUInt32LE(48_000 * 4, 28);
		header.writeUInt16LE(4, 32);
		header.writeUInt16LE(32, 34);
		header.write("data", 36);
		header.writeUInt32LE(pcm.length, 40);
		writeFileSync(`${out}-${kind}.wav`, Buffer.concat([header, pcm]));
		delete result[kind].pcm;
	}
	writeFileSync(`${out}.json`, JSON.stringify(result));
	console.log("wrote", out, JSON.stringify(result.settings), result.userAgent);
} finally {
	await browser.close();
	await server.close();
}
void join;
