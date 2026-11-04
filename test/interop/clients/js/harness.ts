/**
 * Shared Playwright plumbing for the browser clients: serve the built page, launch an engine, and
 * read back the state each role mirrors onto the DOM.
 *
 * Used by `driver.ts` (the interop matrix) and `media.ts` (the media-output and lifecycle checks).
 *
 * @module
 */
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import {
	type Browser,
	type BrowserContext,
	type BrowserContextOptions,
	chromium,
	firefox,
	type Page,
	webkit,
} from "playwright";
import {
	CAPTURE_EVENT,
	type CaptureState,
	CONTROL,
	FIXTURE_EVENT,
	type FixtureState,
	type InteropControl,
	type Resources,
	SAMPLE_EVENT,
	type Sample,
} from "./src/contract";

/**
 * A failed check, named after the property it was measuring.
 *
 * The name is the contract a negative control matches on: injecting a defect has to break the
 * assertion that claims to cover it, not some unrelated wait further down.
 */
export class Failure extends Error {
	readonly assertion: string;

	constructor(assertion: string, detail: string) {
		super(`${assertion}: ${detail}`);
		this.name = "Failure";
		this.assertion = assertion;
	}
}

/** Throw a named {@link Failure} unless `ok`. The detail is only built when it fails. */
export function check(ok: boolean, assertion: string, detail: () => string): void {
	if (!ok) throw new Failure(assertion, detail());
}

/** Errors the page reported, collected so any wait can fail on them instead of timing out. */
export type BrowserErrors = {
	page: string[];
	console: string[];
	/** Console warnings, which fail nothing but can show which path the page took. */
	warnings: string[];
};

/** Keep the UI contract in one place so player markup changes fail clearly. */
export const SELECTORS = {
	watch: "moq-watch",
	ui: "moq-watch-ui",
	pauseControl: 'button.control[aria-label="Pause"]',
	centerPlay: "button.center-play",
} as const;

/** Activate the player's pause button without depending on pointer hit testing. */
export async function pause(page: Page): Promise<void> {
	// Enter focuses and activates the real button even when the chrome auto-hides.
	await page.locator(SELECTORS.ui).locator(SELECTORS.pauseControl).press("Enter");
}

/** How often a wait re-reads the page. */
export const POLL_INTERVAL_MS = 100;

/** Sleep for `ms`. */
export const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Serve the prebuilt page on localhost, a secure context so WebTransport and WebCodecs are enabled.
 *
 * `interop.sh` builds the page into its run directory, so a concurrent run's rebuild cannot empty
 * it mid-load; outside a harness run, it is vite's default `dist/`. `files` serves each path from a
 * file of its own.
 */
export function serve(files: Record<string, string> = {}): { origin: string; stop: () => void } {
	const run = process.env.HARNESS_RUN_DIR;
	const root = run ? join(run, "js-dist") : join(new URL(".", import.meta.url).pathname, "dist");
	const server = Bun.serve({
		port: 0,
		async fetch(req) {
			let path = new URL(req.url).pathname;
			if (path === "/") path = "/index.html";
			const file = Bun.file(files[path] ?? join(root, path));
			if (await file.exists()) return new Response(file);
			return new Response(Bun.file(join(root, "index.html"))); // SPA fallback
		},
	});
	return { origin: `http://localhost:${server.port}`, stop: () => server.stop(true) };
}

/** Build a page URL for a role, with everything after `role` passed through as query parameters. */
export function pageUrl(origin: string, role: string, params: Record<string, string>): string {
	const query = new URLSearchParams({ role, ...params });
	return `${origin}/?${query}`;
}

/** Browser engines the media harness can launch. */
export const ENGINES = ["chromium", "firefox", "webkit"] as const;
/** One of the pinned Playwright engines. */
export type Engine = (typeof ENGINES)[number];

/** Launch an engine with its normal gesture gate and caller-supplied Chromium arguments. */
export function launch(args: string[] = [], engine: Engine = "chromium"): Promise<Browser> {
	// Playwright closes the browser on SIGTERM by default, which ends its sessions without a
	// close. The drivers close it themselves; the harness reaps whatever a signal leaves behind.
	// Full Chromium includes WebTransport and WebCodecs; the headless shell does not.
	if (engine === "chromium")
		return chromium.launch({ channel: "chromium", headless: true, args, handleSIGTERM: false });
	if (engine === "firefox")
		return firefox.launch({
			headless: true,
			handleSIGTERM: false,
			firefoxUserPrefs: { "media.autoplay.default": 1, "media.autoplay.block-webaudio": true },
		});
	// WebKit's WebCodecs decode through the system GStreamer. The Nix dev shell points GStreamer at
	// its own newer plugins, which the system core refuses, leaving WebKit with no decoders at all.
	const env = Object.fromEntries(
		Object.entries(process.env).filter(
			(entry): entry is [string, string] => entry[1] !== undefined && !entry[0].startsWith("GST_PLUGIN"),
		),
	);
	return webkit.launch({ headless: true, handleSIGTERM: false, env });
}

// The console lines that carry a page's state instead of a log message. Pages report rather than
// being read, since Playwright's page reads grant user activation and would unlock Web Audio before
// the deliberate click the gesture checks make.
const REPORTS = [SAMPLE_EVENT, FIXTURE_EVENT, CAPTURE_EVENT];

// The latest report of each kind every page logged, unparsed. Cleared on navigation so a reload
// cannot read the previous document's state.
const reports = new WeakMap<Page, Map<string, string>>();

/** The page's latest report with `prefix`, or undefined when it has reported none (or `null`). */
function report<T>(page: Page, prefix: string): T | undefined {
	const latest = reports.get(page)?.get(prefix);
	return latest === undefined ? undefined : ((JSON.parse(latest) as T | null) ?? undefined);
}

/** Contexts tracing this process, saved by {@link finishTraces} when the run fails. */
const traces: Array<{ context: BrowserContext; name: string }> = [];

/**
 * Start a Playwright trace on the page's context, before it navigates.
 *
 * A no-op outside a harness run: without `HARNESS_RUN_DIR` there is no directory to write to, and a
 * trace only survives a failure anyway. See {@link finishTraces}.
 */
export async function startTrace(page: Page, name: string): Promise<void> {
	if (!process.env.HARNESS_RUN_DIR) return;
	await page.context().tracing.start({ screenshots: true, snapshots: true });
	traces.push({ context: page.context(), name: `${name}-${randomUUID()}` });
}

/** Save a trace per started context into the run directory when `failed`, and discard it otherwise. */
export async function finishTraces(failed: boolean): Promise<void> {
	const run = process.env.HARNESS_RUN_DIR;
	for (const { context, name } of traces) {
		try {
			// `path` is what writes the trace; without it, stop only frees the buffers.
			if (run && failed) await context.tracing.stop({ path: join(run, `${name}.trace.zip`) });
			else await context.tracing.stop();
		} catch {
			// A trace is evidence, never the verdict: a broken context must not mask the failure.
		}
	}
	traces.length = 0;
}

/** Open a page and start collecting its errors, echoing everything it logs.
 *
 * `trace` starts a Playwright trace before the navigation, so a failed run can save it with
 * {@link finishTraces}. The caller decides which pages are worth tracing: a page that streams for
 * the whole run holds its trace in memory, so it is not one.
 */
export async function open(
	browser: Browser,
	url: string,
	label = "page",
	trace = false,
	options?: BrowserContextOptions,
): Promise<[Page, BrowserErrors]> {
	const page = await browser.newPage(options);
	const errors: BrowserErrors = { page: [], console: [], warnings: [] };
	const latest = new Map<string, string>();
	reports.set(page, latest);
	page.on("framenavigated", (frame) => {
		if (frame === page.mainFrame()) latest.clear();
	});
	page.on("console", (message) => {
		const text = message.text();
		const prefix = REPORTS.find((prefix) => text.startsWith(prefix));
		if (prefix) {
			latest.set(prefix, text.slice(prefix.length));
			return;
		}
		console.error(`[${label}] ${text}`);
		if (message.type() === "error") errors.console.push(text);
		if (message.type() === "warning") errors.warnings.push(text);
	});
	page.on("pageerror", (error) => {
		console.error(`[${label} error] ${error.message}`);
		errors.page.push(error.message);
	});
	// A crashed WebKit page never settles a pending call, so close it: that rejects them.
	page.on("crash", () => {
		console.error(`[${label} error] page crashed`);
		errors.page.push("page crashed");
		void page.close().catch(() => {});
	});
	if (trace) await startTrace(page, label);
	await page.goto(url, { waitUntil: "load" });
	return [page, errors];
}

/** Throw everything the page has reported so far, if anything. */
export function throwPageErrors(errors: BrowserErrors): void {
	const messages = [
		...errors.page.map((error) => `page: ${error}`),
		...errors.console.map((error) => `console: ${error}`),
	];
	if (messages.length > 0) throw new Error(messages.join("\n"));
}

/** Wait until the player element exists and has published its first sample. */
export async function waitForWatch(page: Page): Promise<void> {
	const deadline = Date.now() + 30_000;
	while (Date.now() < deadline) {
		if (report(page, SAMPLE_EVENT)) return;
		await sleep(POLL_INTERVAL_MS);
	}
	throw new Error("the player did not publish its first sample");
}

/** Read the latest player sample. Throws until the page has sampled at least once. */
export async function readPlayerState(page: Page): Promise<Sample> {
	const sample = report<Sample>(page, SAMPLE_EVENT);
	if (!sample) throw new Error("the player has not published a sample");
	return sample;
}

/** Read what the fixture publisher says about itself. Throws until it has published anything. */
export async function readFixtureState(page: Page): Promise<FixtureState> {
	const state = report<FixtureState>(page, FIXTURE_EVENT);
	if (!state) throw new Error("the fixture publisher has not published its state");
	return state;
}

/** Read the capture publisher's source state. Throws until it has published any. */
export async function readCaptureState(page: Page): Promise<CaptureState> {
	const state = report<CaptureState>(page, CAPTURE_EVENT);
	if (!state) throw new Error("the publisher has not published its capture state");
	return state;
}

/** Invoke one of the page's {@link InteropControl} commands. */
export async function command<K extends keyof InteropControl>(
	page: Page,
	name: K,
): Promise<Awaited<ReturnType<InteropControl[K]>>> {
	return (await page.evaluate(
		([key, fn]) => {
			const control = (window as unknown as Record<string, Record<string, () => unknown> | undefined>)[key];
			if (!control?.[fn]) throw new Error(`the page exposes no ${fn} command`);
			return control[fn]();
		},
		[CONTROL, name] as const,
	)) as Awaited<ReturnType<InteropControl[K]>>;
}

/** Read the page's live resource counts, which outlive the player element. */
export async function readResources(page: Page): Promise<Resources> {
	const state = await page.evaluate(() => document.body.dataset.interopResources);
	if (!state) throw new Error("the page has not published resource counts");
	return JSON.parse(state) as Resources;
}

/** What to wait for: a predicate over the page state, plus what to say when it never happens. */
export type WaitProps<T> = {
	deadline: number;
	description: string;
	predicate: (state: T) => boolean;
	/** Assertion name for the timeout, when the wait itself is the check. Defaults to "timeout". */
	assertion?: string;
};

/** Poll `read` until `predicate` holds, the deadline passes, or the page reports an error. */
export async function waitFor<T>(
	page: Page,
	errors: BrowserErrors,
	read: (page: Page) => Promise<T>,
	props: WaitProps<T>,
): Promise<T> {
	let last: T | undefined;
	while (Date.now() < props.deadline) {
		throwPageErrors(errors);
		// The page may not have sampled yet; that is indistinguishable from "not there yet" and the
		// deadline is what decides, so keep polling rather than failing on the first read.
		last = await read(page).catch(() => undefined);
		if (last !== undefined && props.predicate(last)) return last;
		await sleep(POLL_INTERVAL_MS);
	}
	throwPageErrors(errors);
	throw new Failure(props.assertion ?? "timeout", `waiting for ${props.description}: ${JSON.stringify(last)}`);
}

/** Poll the player until `predicate` holds. See {@link waitFor}. */
export function waitForState(page: Page, errors: BrowserErrors, props: WaitProps<Sample>): Promise<Sample> {
	return waitFor(page, errors, readPlayerState, props);
}

/** Poll the fixture publisher until `predicate` holds. See {@link waitFor}. */
export function waitForFixture(
	page: Page,
	errors: BrowserErrors,
	props: WaitProps<FixtureState>,
): Promise<FixtureState> {
	return waitFor(page, errors, readFixtureState, props);
}

/** Poll the page's resource counts until `predicate` holds. See {@link waitFor}. */
export function waitForResources(page: Page, errors: BrowserErrors, props: WaitProps<Resources>): Promise<Resources> {
	return waitFor(page, errors, readResources, props);
}
