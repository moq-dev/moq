/**
 * Repeated connection racing through the production `@moq/net` connect path.
 *
 * WebTransport is gated rather than mocked: when refused, the page's `WebTransport` throws as a
 * blocked network does, so the WebSocket fallback wins after whatever head start the racing
 * policy grants. Every dialed URL carries `marker` in its query, which the driver counts in a
 * heap snapshot once every session has closed. `redact` logs drop the query, so no console
 * message can hold a marker.
 */
import * as Moq from "@moq/net";
import "./observe.ts";

/**
 * What the WebTransport arm can do and which fallback the dial uses.
 *
 * - `websocket`: WebTransport refused; the default fallback wins every dial.
 * - `webtransport`: WebTransport open, fallback enabled; the control with no WebSocket win.
 * - `sticky`: per URL, a WebSocket win, then two dials with WebTransport open again.
 * - `alternate`: WebTransport refused; the fallback dials its own URL.
 * - `disabled`: WebTransport open, fallback disabled.
 */
export type Mode = "websocket" | "webtransport" | "sticky" | "alternate" | "disabled";

export interface Racing {
	url: string;
	marker: string;
	mode: Mode;
	/** Distinct URLs, spread over `lanes`. */
	urls: number;
	/** Sequential dials per URL, each closing before the next. */
	reconnects: number;
	/** Padding characters in each URL's query. */
	length: number;
	/** Whether every reconnect changes the query, as a rotated token does. */
	rotate: boolean;
	/**
	 * URLs dialed at once. More than one lets a large sweep finish, but its dials queue on one
	 * another (a browser opens one WebSocket to an address at a time), so their latency is not
	 * a single dial's.
	 */
	lanes: number;
}

/** One dial: which reconnect of its URL it was, the transport that won, and how long it took. */
export interface Dial {
	attempt: number;
	transport: string;
	ms: number;
	/** Characters in the dialed URL, the key a WebSocket win records. */
	chars: number;
}

// URL indices whose WebTransport dials are refused. Numbers, so the gate holds no marker string.
const refused = new Set<number>();
const Native = globalThis.WebTransport;
class Gated extends Native {
	constructor(url: string | URL, options?: WebTransportOptions) {
		const index = /\/racing\/(\d+)$/.exec(new URL(url).pathname);
		if (index && refused.has(Number(index[1]))) throw new TypeError("WebTransport refused by the benchmark");
		super(url, options);
	}
}
globalThis.WebTransport = Gated;

function address(config: Racing, index: number, attempt: number): URL {
	const url = new URL(`racing/${index}`, `${config.url}/`);
	// The marker leads the query so a heap snapshot's string names, which V8 truncates, still hold it.
	url.searchParams.set("m", `${config.marker}-${index}-${config.rotate ? attempt : 0}`);
	url.searchParams.set("pad", "x".repeat(config.length));
	return url;
}

async function dial(config: Racing, index: number, attempt: number): Promise<Dial> {
	const refuse =
		config.mode === "websocket" || config.mode === "alternate" || (config.mode === "sticky" && attempt === 0);
	if (refuse) refused.add(index);
	else refused.delete(index);
	const url = address(config, index, attempt);
	const websocket: Moq.Connection.WebSocketProps =
		config.mode === "disabled"
			? { enabled: false }
			: config.mode === "alternate"
				? { url: new URL(`fallback/${index}`, `${config.url}/`) }
				: {};
	const start = performance.now();
	const connection = await Moq.Connection.connect({ url, websocket });
	const ms = performance.now() - start;
	const transport = connection.transport;
	await connection.close();
	await connection.closed;
	return { attempt, transport, ms, chars: url.href.length };
}

/** Dials every URL `reconnects` times, URLs spread over parallel lanes and each URL's dials in order. */
async function run(config: Racing): Promise<Dial[]> {
	// A sticky URL races a WebSocket win, then two dials with WebTransport open again.
	const reconnects = config.mode === "sticky" ? 3 : config.reconnects;
	const dials: Dial[] = [];
	let next = 0;
	const lane = async () => {
		for (let index = next++; index < config.urls; index = next++)
			for (let attempt = 0; attempt < reconnects; attempt++) dials.push(await dial(config, index, attempt));
	};
	await Promise.all(Array.from({ length: Math.min(config.lanes, config.urls) }, lane));
	refused.clear();
	return dials;
}

declare global {
	interface Window {
		racing: { run: typeof run };
	}
}
window.racing = { run };
