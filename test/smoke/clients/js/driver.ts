/**
 * Drives a headless Chromium (channel "chromium" for WebTransport + WebCodecs)
 * against the vite-built page (dist/). publish streams fake camera/microphone
 * input until killed; subscribe verifies rendered playback, pause/resume, and
 * optionally browser-to-browser audio.
 *
 *     bun driver.ts publish   --url http://127.0.0.1:4443 --broadcast b.hang
 *     bun driver.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20 [--expect-audio]
 *
 * @module
 */
import { join } from "node:path";
import { parseArgs } from "node:util";
import { chromium, type Page } from "playwright";

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		timeout: { type: "string", default: "20" },
		"expect-audio": { type: "boolean", default: false },
	},
});

const role = positionals[0];
const url = values.url;
const broadcast = values.broadcast;
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;
const expectAudio = values["expect-audio"] ?? false;
if (
	(role !== "publish" && role !== "subscribe") ||
	!url ||
	!broadcast ||
	!Number.isFinite(timeoutMs) ||
	timeoutMs <= 0 ||
	(expectAudio && role !== "subscribe")
) {
	console.error("usage: driver.ts publish|subscribe --url U --broadcast B [--timeout S>0] [--expect-audio]");
	process.exit(2);
}

type PlayerState = {
	videoFrames: number;
	videoTimestamp?: number;
	painted: boolean;
	audioBytes: number;
	audioContext?: string;
	hasAudio: boolean;
	paused: boolean;
	pausedAttribute: boolean;
	controlLabel?: string;
	centerPlayVisible: boolean;
};

type BrowserErrors = {
	page: string[];
	console: string[];
};

type WaitForStateProps = {
	deadline: number;
	description: string;
	predicate: (state: PlayerState) => boolean;
};

// Keep the UI contract in one place so player markup changes fail clearly.
const SELECTORS = {
	watch: "moq-watch",
	ui: "moq-watch-ui",
	control: "button.control[aria-label]",
	pauseControl: 'button.control[aria-label="Pause"]',
	centerPlay: "button.center-play",
} as const;
const POLL_INTERVAL_MS = 100;
const PAUSE_STABILITY_MS = 750;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function throwPageErrors(errors: BrowserErrors): void {
	const messages = [
		...errors.page.map((error) => `page: ${error}`),
		...errors.console.map((error) => `console: ${error}`),
	];
	if (messages.length > 0) throw new Error(messages.join("\n"));
}

async function readPlayerState(page: Page): Promise<PlayerState> {
	return page.evaluate((selectors) => {
		type Signal<T> = { peek(): T };
		type Watch = HTMLElement & {
			backend: {
				paused: Signal<boolean>;
				video: {
					stats: Signal<{ frameCount?: number } | undefined>;
					timestamp: Signal<number | undefined>;
				};
				audio: {
					stats: Signal<{ bytesReceived?: number } | undefined>;
					context: Signal<AudioContext | undefined>;
				};
			};
			broadcast: { catalog: Signal<{ audio?: unknown } | undefined> };
		};

		const watch = document.querySelector(selectors.watch) as Watch | null;
		const canvas = watch?.querySelector("canvas");
		let painted = false;
		if (canvas && canvas.width > 0 && canvas.height > 0) {
			const pixels = canvas.getContext("2d")?.getImageData(0, 0, canvas.width, canvas.height).data;
			if (pixels) {
				// The smoke sources are test patterns. Sample at most about 1,000
				// pixels and require actual color rather than the renderer's black fill.
				const stride = Math.max(4, Math.floor(pixels.length / 4000) * 4);
				for (let i = 0; i < pixels.length; i += stride) {
					if (pixels[i] + pixels[i + 1] + pixels[i + 2] > 12) {
						painted = true;
						break;
					}
				}
			}
		}

		const ui = document.querySelector(selectors.ui);
		const control = ui?.shadowRoot?.querySelector<HTMLButtonElement>(selectors.control);
		const centerPlay = ui?.shadowRoot?.querySelector<HTMLButtonElement>(selectors.centerPlay);

		return {
			videoFrames: watch?.backend.video.stats.peek()?.frameCount ?? 0,
			videoTimestamp: watch?.backend.video.timestamp.peek(),
			painted,
			audioBytes: watch?.backend.audio.stats.peek()?.bytesReceived ?? 0,
			audioContext: watch?.backend.audio.context.peek()?.state,
			hasAudio: watch?.broadcast.catalog.peek()?.audio !== undefined,
			paused: watch?.backend.paused.peek() ?? false,
			pausedAttribute: watch?.hasAttribute("paused") ?? false,
			controlLabel: control?.getAttribute("aria-label") ?? undefined,
			centerPlayVisible: centerPlay ? getComputedStyle(centerPlay).display !== "none" : false,
		};
	}, SELECTORS);
}

async function waitForState(page: Page, errors: BrowserErrors, props: WaitForStateProps): Promise<PlayerState> {
	let last = await readPlayerState(page);
	while (Date.now() < props.deadline) {
		throwPageErrors(errors);
		if (props.predicate(last)) return last;
		await sleep(POLL_INTERVAL_MS);
		last = await readPlayerState(page);
	}
	throwPageErrors(errors);
	throw new Error(`timed out waiting for ${props.description}: ${JSON.stringify(last)}`);
}

async function waitForStablePause(page: Page, errors: BrowserErrors, deadline: number): Promise<PlayerState> {
	let previous = await readPlayerState(page);
	let stableSince = Date.now();
	while (Date.now() < deadline) {
		throwPageErrors(errors);
		await sleep(POLL_INTERVAL_MS);
		const current = await readPlayerState(page);
		const stable =
			current.videoFrames === previous.videoFrames &&
			current.videoTimestamp === previous.videoTimestamp &&
			(!expectAudio || current.audioBytes === previous.audioBytes);
		if (!stable) stableSince = Date.now();
		if (stable && Date.now() - stableSince >= PAUSE_STABILITY_MS) return current;
		previous = current;
	}
	throwPageErrors(errors);
	throw new Error(`playback did not stop after pause: ${JSON.stringify(previous)}`);
}

// Serve the prebuilt page on localhost (a secure context, so WebTransport /
// WebCodecs are enabled).
const root = join(new URL(".", import.meta.url).pathname, "dist");
const server = Bun.serve({
	port: 0,
	async fetch(req) {
		let path = new URL(req.url).pathname;
		if (path === "/") path = "/index.html";
		const file = Bun.file(join(root, path));
		if (await file.exists()) return new Response(file);
		return new Response(Bun.file(join(root, "index.html"))); // SPA fallback
	},
});
const pageUrl = `http://localhost:${server.port}/?role=${role}&url=${encodeURIComponent(url)}&broadcast=${encodeURIComponent(broadcast)}`;

const browser = await chromium.launch({
	channel: "chromium", // full Chromium (new headless); the headless shell lacks these APIs
	headless: true,
	args: [
		"--use-fake-device-for-media-stream",
		"--use-fake-ui-for-media-stream",
		"--autoplay-policy=no-user-gesture-required",
	],
});

let code = 1;
try {
	const page = await browser.newPage();
	const errors: BrowserErrors = { page: [], console: [] };
	page.on("console", (message) => {
		console.error(`[page] ${message.text()}`);
		if (message.type() === "error") errors.console.push(message.text());
	});
	page.on("pageerror", (error) => {
		console.error(`[page error] ${error.message}`);
		errors.page.push(error.message);
	});
	await page.goto(pageUrl, { waitUntil: "load" });

	if (role === "publish") {
		console.error(`publishing ${broadcast} (fake camera + microphone) to ${url}`);
		await new Promise(() => {}); // stream until the orchestrator kills us
	} else {
		const start = Date.now();
		const startupDeadline = start + timeoutMs;
		let reloaded = false;
		let playing: PlayerState | undefined;
		while (Date.now() < startupDeadline) {
			throwPageErrors(errors);
			const state = await readPlayerState(page);
			if (state.videoTimestamp !== undefined && state.painted && state.controlLabel === "Pause") {
				playing = state;
				break;
			}
			// Retry once after the publisher has had time to announce. This preserves
			// the existing startup tolerance while the interaction checks below stay strict.
			if (!reloaded && Date.now() - start > timeoutMs / 2) {
				reloaded = true;
				await page.reload({ waitUntil: "load" });
			}
			await sleep(POLL_INTERVAL_MS);
		}
		if (!playing) {
			const state = await readPlayerState(page);
			throw new Error(`timed out waiting for rendered video: ${JSON.stringify(state)}`);
		}

		const interactionDeadline = Date.now() + timeoutMs;
		if (expectAudio) {
			playing = await waitForState(page, errors, {
				deadline: interactionDeadline,
				description: "browser audio",
				predicate: (state) => state.hasAudio && state.audioBytes > 0 && state.audioContext === "running",
			});
		}

		// The chrome auto-hides while playing. Pointer activity reveals the real
		// control, then the click must flow through the public player API.
		await page.dispatchEvent(SELECTORS.ui, "pointermove");
		await page.locator(SELECTORS.ui).locator(SELECTORS.pauseControl).click();
		await waitForState(page, errors, {
			deadline: interactionDeadline,
			description: "paused player UI",
			predicate: (state) =>
				state.paused && state.pausedAttribute && state.controlLabel === "Play" && state.centerPlayVisible,
		});
		const paused = await waitForStablePause(page, errors, interactionDeadline);
		if (!paused.painted) throw new Error(`pause cleared the preview frame: ${JSON.stringify(paused)}`);

		await page.locator(SELECTORS.ui).locator(SELECTORS.centerPlay).click();
		const resumed = await waitForState(page, errors, {
			deadline: interactionDeadline,
			description: "resumed playback",
			predicate: (state) =>
				!state.paused &&
				!state.pausedAttribute &&
				state.controlLabel === "Pause" &&
				!state.centerPlayVisible &&
				state.videoFrames > paused.videoFrames &&
				state.videoTimestamp !== undefined &&
				state.videoTimestamp > (paused.videoTimestamp ?? -1) &&
				(!expectAudio || state.audioBytes > paused.audioBytes),
		});

		throwPageErrors(errors);
		console.error(
			`rendered, paused, and resumed ${broadcast}: video=${resumed.videoFrames} frames` +
				(expectAudio ? ` audio=${resumed.audioBytes} bytes` : ""),
		);
		code = 0;
	}
} finally {
	await browser.close().catch(() => {});
	server.stop(true);
}
process.exit(code);
