// The obs-websocket 5 protocol: https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md

import { Duration, timeout } from "@moq/clock";
import { type Dispose, type GetPromise, Once } from "@moq/signals";
import * as z from "zod/mini";
import { ConnectError, RequestError } from "./error.ts";

const OP_HELLO = 0;
const OP_IDENTIFY = 1;
const OP_IDENTIFIED = 2;
const OP_EVENT = 5;
const OP_REQUEST = 6;
const OP_RESPONSE = 7;

// The only RPC version obs-websocket 5 has shipped.
const RPC_VERSION = 1;

// WebSocketCloseCode values that mean the handshake itself failed.
const CLOSE_AUTHENTICATION_FAILED = 4009;

// EventSubscription bits: General (ExitStarted), Config (CurrentProfileChanged), Outputs (StreamStateChanged).
const SUBSCRIPTIONS = 1 | 2 | 64;

// obs-websocket sends Hello as soon as the socket opens; anything slower is some other server.
const HELLO_TIMEOUT = Duration.secs(5);

const Envelope = z.object({ op: z.number(), d: z.record(z.string(), z.unknown()) });
const Hello = z.object({
	rpcVersion: z.number(),
	authentication: z.optional(z.object({ challenge: z.string(), salt: z.string() })),
});
const Event = z.object({ eventType: z.string(), eventData: z.optional(z.record(z.string(), z.unknown())) });
const Response = z.object({
	requestType: z.string(),
	requestId: z.string(),
	requestStatus: z.object({ result: z.boolean(), code: z.number(), comment: z.optional(z.string()) }),
	responseData: z.optional(z.unknown()),
});

type Pending = { resolve: (data: unknown) => void; reject: (error: Error) => void };

/** One identified obs-websocket session. */
export class Session {
	#socket: WebSocket;
	#pending = new Map<string, Pending>();
	#next = 0;
	#events = new Map<string, Set<(data: Record<string, unknown>) => void>>();
	#closed = new Once<Error | null>();

	/** Settles with `null` after {@link close}, or the error that ended the session. */
	readonly closed: GetPromise<Error | null> = this.#closed;

	private constructor(socket: WebSocket) {
		this.#socket = socket;
		socket.addEventListener("message", (event) => this.#message(event.data));
		socket.addEventListener("close", (event) => {
			this.#finish(
				new Error(`OBS closed the connection (code ${event.code})${event.reason ? `: ${event.reason}` : ""}`),
			);
		});
	}

	/** Opens and identifies a session, or throws a {@link ConnectError}. */
	static async connect(url: URL, password: string | undefined): Promise<Session> {
		let socket: WebSocket;
		try {
			socket = new WebSocket(url, "obswebsocket.json");
		} catch (error) {
			// The constructor throws only for a URL the page may not open, such as ws:// from HTTPS in Safari.
			throw new ConnectError("blocked", `the browser refused to open ${url}: ${String(error)}`);
		}

		try {
			await handshake(socket, password);
			return new Session(socket);
		} catch (error) {
			socket.close();
			throw error;
		}
	}

	/** Sends a request and resolves with its response data, or rejects with a {@link RequestError}. */
	request(type: string, data?: Record<string, unknown>): Promise<unknown> {
		const closed = this.#closed.peek();
		if (closed !== undefined) return Promise.reject(closed ?? new Error("the OBS connection is closed"));

		const requestId = String(this.#next++);
		const { promise, resolve, reject } = Promise.withResolvers<unknown>();
		this.#pending.set(requestId, { resolve, reject });
		this.#socket.send(JSON.stringify({ op: OP_REQUEST, d: { requestType: type, requestId, requestData: data } }));
		return promise;
	}

	/** Calls `fn` with each event of `type` until disposed. */
	on(type: string, fn: (data: Record<string, unknown>) => void): Dispose {
		let set = this.#events.get(type);
		if (!set) {
			set = new Set();
			this.#events.set(type, set);
		}
		set.add(fn);
		return () => set.delete(fn);
	}

	/** Closes the session; pending requests reject. */
	close(): void {
		this.#finish(null);
		this.#socket.close();
	}

	#fail(message: string) {
		this.#finish(new Error(message));
		this.#socket.close();
	}

	#finish(error: Error | null) {
		if (this.#closed.peek() !== undefined) return;
		this.#closed.set(error);
		const reason = error ?? new Error("the OBS connection is closed");
		for (const pending of this.#pending.values()) pending.reject(reason);
		this.#pending.clear();
		this.#events.clear();
	}

	#message(raw: unknown) {
		const message = parse(raw);
		if (!message) return this.#fail("OBS sent a malformed message");

		if (message.op === OP_RESPONSE) {
			const response = Response.safeParse(message.d);
			if (!response.success) return this.#fail("OBS sent a malformed response");
			const pending = this.#pending.get(response.data.requestId);
			if (!pending) return;
			this.#pending.delete(response.data.requestId);
			const { result, code, comment } = response.data.requestStatus;
			if (result) pending.resolve(response.data.responseData);
			else pending.reject(new RequestError(response.data.requestType, code, comment));
		} else if (message.op === OP_EVENT) {
			const event = Event.safeParse(message.d);
			if (!event.success) return this.#fail("OBS sent a malformed event");
			for (const fn of this.#events.get(event.data.eventType) ?? []) fn(event.data.eventData ?? {});
		}
	}
}

function parse(raw: unknown): z.infer<typeof Envelope> | undefined {
	if (typeof raw !== "string") return undefined;
	try {
		const result = Envelope.safeParse(JSON.parse(raw));
		return result.success ? result.data : undefined;
	} catch {
		return undefined;
	}
}

// Waits for Hello, answers with Identify, and resolves with the socket once OBS sends Identified.
async function handshake(socket: WebSocket, password: string | undefined): Promise<void> {
	const opened = Promise.withResolvers<void>();
	const identified = Promise.withResolvers<void>();
	// A close before the socket opens rejects both; only `opened` is awaited then.
	identified.promise.catch(() => {});
	let open = false;

	const onOpen = () => {
		open = true;
		opened.resolve();
	};
	const onClose = (event: CloseEvent) => {
		const error = open ? closedAfterOpen(event) : undefined;
		if (error) {
			opened.reject(error);
			identified.reject(error);
		} else {
			// Closed before it opened: the browser gives no reason, so ask it whether the user said no.
			void unreachable(socket.url).then((error) => {
				opened.reject(error);
				identified.reject(error);
			});
		}
	};
	const onMessage = (event: MessageEvent) => {
		void (async () => {
			const message = parse(event.data);
			if (message?.op === OP_HELLO) {
				const hello = Hello.safeParse(message.d);
				if (!hello.success || hello.data.rpcVersion < RPC_VERSION) {
					throw new ConnectError("unsupported", "OBS sent an obs-websocket Hello this client cannot read");
				}
				let authentication: string | undefined;
				if (hello.data.authentication) {
					if (password === undefined) throw new ConnectError("refused", "OBS requires a password");
					const { challenge, salt } = hello.data.authentication;
					authentication = await sha256(`${await sha256(password + salt)}${challenge}`);
				}
				socket.send(
					JSON.stringify({
						op: OP_IDENTIFY,
						d: { rpcVersion: RPC_VERSION, authentication, eventSubscriptions: SUBSCRIPTIONS },
					}),
				);
			} else if (message?.op === OP_IDENTIFIED) {
				identified.resolve();
			} else {
				throw new ConnectError("unsupported", "the server is not obs-websocket 5");
			}
		})().catch((error: Error) => identified.reject(error));
	};

	socket.addEventListener("open", onOpen);
	socket.addEventListener("close", onClose);
	socket.addEventListener("message", onMessage);
	try {
		await opened.promise;
		try {
			await timeout(HELLO_TIMEOUT, identified.promise);
		} catch (error) {
			if (error instanceof ConnectError) throw error;
			throw new ConnectError("unsupported", "the server did not complete the obs-websocket 5 handshake");
		}
	} finally {
		socket.removeEventListener("open", onOpen);
		socket.removeEventListener("close", onClose);
		socket.removeEventListener("message", onMessage);
	}
}

function closedAfterOpen(event: CloseEvent): ConnectError {
	if (event.code === CLOSE_AUTHENTICATION_FAILED) return new ConnectError("refused", "OBS rejected the password");
	return new ConnectError(
		"unsupported",
		`OBS closed the handshake (code ${event.code})${event.reason ? `: ${event.reason}` : ""}`,
	);
}

// Chrome 147+ and Firefox ask before a page outside loopback opens a WebSocket to it, under
// `loopback-network` (`local-network-access` in Chrome 142-144). A refusal, or a prompt left
// unanswered until the browser gives up, fails the socket like an empty port does.
async function unreachable(url: string): Promise<ConnectError> {
	const state = await loopbackPermission();
	if (state === "denied" || (state === "prompt" && !loopbackPage())) {
		return new ConnectError("denied", "the browser was not allowed to reach apps on this device");
	}
	// Safari has no such permission; it refuses ws:// from any HTTPS page as mixed content.
	const https = state === undefined && globalThis.location?.protocol === "https:";
	return new ConnectError(
		"unreachable",
		`nothing answered at ${url}; is OBS running with its WebSocket server on?${https ? " Safari cannot reach it from an HTTPS page." : ""}`,
	);
}

async function loopbackPermission(): Promise<PermissionState | undefined> {
	for (const name of ["loopback-network", "local-network-access"]) {
		try {
			const status = await globalThis.navigator?.permissions?.query({ name: name as PermissionName });
			if (status) return status.state;
		} catch {
			// This browser does not know the permission name; try the next one.
		}
	}
	return undefined;
}

// A page served from loopback needs no permission to reach it.
function loopbackPage(): boolean {
	const host = globalThis.location?.hostname;
	if (host === undefined) return true;
	return host === "localhost" || host.endsWith(".localhost") || host.startsWith("127.") || host === "[::1]";
}

async function sha256(input: string): Promise<string> {
	const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input)));
	let binary = "";
	for (const byte of digest) binary += String.fromCharCode(byte);
	return btoa(binary);
}
