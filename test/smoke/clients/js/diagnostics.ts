import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import type { ConsoleMessage, Page, Request, Response } from "playwright";

interface Exchange {
	method: string;
	url: string;
	resourceType: string;
	requestHeaders: Record<string, string>;
	response?: {
		status: number;
		statusText: string;
		headers: Record<string, string>;
		body?: string;
		bodyError?: string;
	};
	failure?: string;
}

interface Diagnostic {
	role: string;
	variant: string;
	pageUrl: string;
	error?: string;
	requests: Exchange[];
	console: Array<{ type: string; text: string; location: ReturnType<ConsoleMessage["location"]> }>;
	pageErrors: string[];
}

const CDN = "https://cdn.jsdelivr.net/";
const MAX_BODY = 256 * 1024;

export function responseBody(body: Buffer): { body?: string; bodyError?: string } {
	if (body.byteLength > MAX_BODY) return { bodyError: `body exceeds ${MAX_BODY} bytes` };
	return { body: body.toString("base64") };
}

export class Diagnostics {
	readonly #data: Diagnostic;
	readonly #requests = new Map<Request, Exchange>();
	readonly #pending: Promise<void>[] = [];

	constructor(page: Page, role: string, variant: string, pageUrl: string) {
		this.#data = { role, variant, pageUrl, requests: [], console: [], pageErrors: [] };
		page.on("request", (request) => this.#request(request));
		page.on("response", (response) => this.#response(response));
		page.on("requestfinished", (request) => this.#body(request));
		page.on("requestfailed", (request) => {
			this.#exchange(request).failure = request.failure()?.errorText ?? "unknown request failure";
			this.#body(request);
		});
		page.on("console", (message) => this.console(message));
		page.on("pageerror", (error) => this.#data.pageErrors.push(error.stack ?? error.message));
	}

	console(message: ConsoleMessage): void {
		this.#data.console.push({ type: message.type(), text: message.text(), location: message.location() });
	}

	async write(dir: string, name: string, error?: unknown): Promise<string> {
		this.#data.error =
			error instanceof Error ? (error.stack ?? error.message) : error === undefined ? undefined : String(error);
		await Promise.allSettled(this.#pending);
		for (const exchange of this.#data.requests) {
			const response = exchange.response;
			if (
				exchange.url.startsWith(CDN) &&
				response &&
				response.body === undefined &&
				response.bodyError === undefined
			) {
				response.bodyError = "body incomplete when the cell ended";
			}
		}
		await mkdir(dir, { recursive: true });
		const path = join(dir, `${name}.json`);
		await Bun.write(path, `${JSON.stringify(this.#data, undefined, 2)}\n`);
		return path;
	}

	#request(request: Request): Exchange {
		const exchange: Exchange = {
			method: request.method(),
			url: request.url(),
			resourceType: request.resourceType(),
			requestHeaders: request.headers(),
		};
		this.#requests.set(request, exchange);
		this.#data.requests.push(exchange);
		return exchange;
	}

	#response(response: Response): void {
		this.#exchange(response.request()).response = {
			status: response.status(),
			statusText: response.statusText(),
			headers: response.headers(),
		};
	}

	// Read only after the request ends, so a stalled CDN body cannot hang write().
	#body(request: Request): void {
		const exchange = this.#exchange(request);
		if (!exchange.url.startsWith(CDN)) return;
		this.#pending.push(
			(async () => {
				const meta = exchange.response;
				if (!meta) return;
				try {
					const response = await request.response();
					if (response) Object.assign(meta, responseBody(await response.body()));
				} catch (error) {
					meta.bodyError = error instanceof Error ? error.message : String(error);
				}
			})(),
		);
	}

	#exchange(request: Request): Exchange {
		return this.#requests.get(request) ?? this.#request(request);
	}
}
