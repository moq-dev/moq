import { error, ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import type * as Path from "../path.ts";
import { Reader, type Stream, type Writer } from "../stream.ts";
import type { Session } from "./adapter.ts";
import { type RequestCondition, type RequestKind, toRequestCode } from "./error.ts";
import { FetchError } from "./fetch.ts";
import { GoAway } from "./goaway.ts";
import * as Message from "./message.ts";
import { Parameters } from "./parameters.ts";
import { PublishError } from "./publish.ts";
import { PublishNamespaceError } from "./publish_namespace.ts";
import { RequestError } from "./request.ts";
import { SubscribeError } from "./subscribe.ts";
import { SubscribeNamespaceError } from "./subscribe_namespace.ts";
import { MalformedAuthToken } from "./token.ts";
import { TRACK_STATUS_ERROR_14 } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

/** Whether the draft treats the requester finishing its messages as cancellation. */
function finCancels(version: IetfVersion): boolean {
	return (
		version === Version.DRAFT_14 ||
		version === Version.DRAFT_15 ||
		version === Version.DRAFT_16 ||
		version === Version.DRAFT_17 ||
		version === Version.DRAFT_18
	);
}

/**
 * Why a request is refused, and which one: only drafts 14-16 name the Request ID.
 * @internal
 */
export interface Refusal {
	requestId?: bigint;
	condition: RequestCondition;
	reasonPhrase: string;
}

// Each draft-14 error message has the REQUEST_ERROR body that draft-15 folded them into.
const ERROR_14: Record<RequestKind, number> = {
	subscribe: SubscribeError.id,
	fetch: FetchError.id,
	publish: PublishError.id,
	publish_namespace: PublishNamespaceError.id,
	subscribe_namespace: SubscribeNamespaceError.id,
	track_status: TRACK_STATUS_ERROR_14,
};

async function writeRefusal(w: Writer, version: IetfVersion, kind: RequestKind, refusal: Refusal): Promise<void> {
	const err = new RequestError({
		requestId: version <= Version.DRAFT_16 ? refusal.requestId : undefined,
		errorCode: toRequestCode(refusal.condition, kind, version),
		reasonPhrase: refusal.reasonPhrase,
	});
	await Message.write(w, version === Version.DRAFT_14 ? ERROR_14[kind] : RequestError.id, err, version);
}

/**
 * Refuse a request and end its stream.
 * @internal
 */
export async function refuse(stream: Stream, version: IetfVersion, kind: RequestKind, refusal: Refusal): Promise<void> {
	await writeRefusal(stream.writer, version, kind, refusal);
	stream.close();
}

/**
 * Refuse a REQUEST_UPDATE, leaving its request standing. Draft 14 answers no update.
 * @internal
 */
export async function refuseUpdate(
	w: Writer,
	version: IetfVersion,
	kind: RequestKind,
	refusal: Refusal,
): Promise<void> {
	if (version !== Version.DRAFT_14) await writeRefusal(w, version, kind, refusal);
}

/**
 * Refuse an unrecognized reserved namespace before it reaches the application.
 *
 * @internal
 */
export async function refuseReserved(request: RequestStream): Promise<void> {
	await request.refuse("subscribe", {
		condition: "does_not_exist",
		reasonPhrase: "reserved namespace does not exist",
	});
}

/**
 * The GOAWAYs the peer sent on one request stream. `client` is our own role: only a server
 * may name a new session.
 * @internal
 */
type GoAways = { client: boolean; seen: boolean };

// One reader spans the initial response and later updates on the same request.
const goaways = new WeakMap<Reader, GoAways>();

/**
 * Read the type of a request stream's next message, or undefined once the peer finishes.
 * From draft 18 on, the peer may send one GOAWAY on a request stream, consumed here; a second,
 * or a client's naming a new session, is a protocol violation.
 * @internal
 */
export async function nextMessage(reader: Reader, version: IetfVersion, client: boolean): Promise<number | undefined> {
	let goaway = goaways.get(reader);
	if (!goaway) {
		goaway = { client, seen: false };
		goaways.set(reader, goaway);
	}
	try {
		for (;;) {
			if (await reader.done()) return undefined;
			const type = await reader.u53();
			if (type !== GoAway.id || version < Version.DRAFT_18) return type;
			if (goaway.seen) throw new ProtocolViolation("second GOAWAY on a request stream");
			goaway.seen = true;
			// Draft-18's Request ID belongs on the control stream only.
			const msg = await GoAway.decode(reader, version);
			if (msg.requestId !== undefined) throw new ProtocolViolation("request stream GOAWAY carries a Request ID");
			if (!goaway.client && msg.newSessionUri !== "")
				throw new ProtocolViolation("client GOAWAY names a new session");
		}
	} catch (err) {
		throw malformed(err, "request stream message");
	}
}

/**
 * Decode a namespace update the peer sent on its SUBSCRIBE_NAMESPACE.
 * @internal
 */
export async function namespaceUpdate(
	framed: Framed,
	version: IetfVersion,
): Promise<{ requestId: bigint; prefix?: Path.Valid; refused?: RequestCondition }> {
	if (framed.type !== 0x02) throw new ProtocolViolation("expected namespace REQUEST_UPDATE");
	try {
		return await Message.decode(framed.reader, async (body) => {
			const requestId = await body.u62();
			// Drafts 14-16 define no namespace update. The update's own Request ID is all its
			// refusal names; the target is what the adapter routed it by.
			if (version <= Version.DRAFT_16) {
				await body.readAll();
				return { requestId, refused: "not_supported" };
			}
			const params = await Parameters.decode(body, version, "namespace-update");
			return {
				requestId,
				prefix: params.namespacePrefix,
				refused: undefined,
			};
		});
	} catch (err) {
		if (err instanceof MalformedAuthToken && err.requestId !== undefined)
			return { requestId: err.requestId, refused: "malformed_auth_token" };
		throw malformed(err, "namespace REQUEST_UPDATE");
	}
}

/**
 * One message off a request stream, read whole so a taker that stops partway consumes nothing.
 * @internal
 */
export interface Framed {
	type: number;
	/** The length-prefixed body, for the message's own `decode`. */
	reader: Reader;
}

/**
 * The single owner of one request stream, after its request message.
 *
 * Every read goes through it. From the first wait on it keeps reading, consuming GOAWAYs and
 * acting on the end as they arrive, with the next message held in one slot until taken, so no
 * reader is left detached. Every close goes through it once, and a protocol violation closes
 * the session with its code once.
 * @internal
 */
export class RequestStream {
	/** Where answers to the request go. */
	readonly writer: Writer;
	readonly #stream: Stream;
	readonly #session: Session;
	readonly #quic?: WebTransport;
	// The next message, read ahead of whoever takes it. Unset until someone first waits.
	#ahead?: Promise<Framed | undefined>;
	#taking = false;
	#closed = false;
	#violated = false;
	readonly #ended: Promise<void>;
	#end!: () => void;

	/** Own `stream` for a request on `session`, closing `quic` over the peer's violations. */
	constructor(stream: Stream, session: Session, quic?: WebTransport) {
		this.#stream = stream;
		this.writer = stream.writer;
		this.#session = session;
		this.#quic = quic;
		this.#ended = new Promise((resolve) => {
			this.#end = resolve;
		});
	}

	/**
	 * Settles once the peer ends the request, by FIN where that cancels or by resetting either
	 * direction, or once it closes here or over a violation. Never rejects.
	 */
	get ended(): Promise<void> {
		void this.#read();
		return this.#ended;
	}

	/** Take the next message, or undefined once the peer finishes. One taker at a time. */
	async next(): Promise<Framed | undefined> {
		if (this.#taking) throw new Error("request stream already being read");
		this.#taking = true;
		try {
			const framed = await this.#read();
			if (framed !== undefined) this.#ahead = this.#readAhead();
			return framed;
		} finally {
			this.#taking = false;
		}
	}

	/** Refuse the request and end it. */
	async refuse(kind: RequestKind, refusal: Refusal): Promise<void> {
		await writeRefusal(this.writer, this.#session.version, kind, refusal);
		this.close();
	}

	/**
	 * Cancel the request but keep reading, so {@link next} reports when the peer ends its
	 * half: FIN where that cancels, a CANCELLED reset otherwise.
	 */
	cancel(): void {
		if (this.#closed) return;
		if (finCancels(this.#session.version)) this.writer.close();
		else this.writer.reset(new StreamError(StreamCode.Cancel, { message: "cancel" }));
	}

	/** End the request cleanly: FIN, and stop reading. Only the first close counts. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#stream.close();
		this.#end();
	}

	/**
	 * Reset the request with `err`. A protocol violation closes the session with its code
	 * first, even after the request ended. Only the first close of each counts.
	 */
	abort(err: unknown): void {
		const e = error(err);
		if (e instanceof ProtocolViolation && !this.#violated) {
			this.#violated = true;
			// Before the session's own close, which would send a clean code.
			try {
				this.#quic?.close({ closeCode: e.code, reason: e.message });
			} catch {
				// Already closed.
			}
			this.#session.close();
		}
		if (this.#closed) return;
		this.#closed = true;
		this.#stream.abort(e);
		this.#end();
	}

	/** Await `task`, which owns the request: whatever it throws ends the request, and so does returning. */
	async serve(task: Promise<void>): Promise<void> {
		try {
			await task;
		} catch (err) {
			this.abort(err);
			throw err;
		}
		this.close();
	}

	// Start reading on the first wait.
	#read(): Promise<Framed | undefined> {
		if (this.#ahead === undefined) {
			const version = this.#session.version;
			this.#ahead = this.#readAhead();
			// The read ahead stops at an untaken message, so watch for the peer's end apart from it.
			this.#stream.reader.closed.then(
				() => {
					if (finCancels(version)) this.#end();
				},
				() => this.#end(),
			);
			// Drafts 14-16 adapt the request onto the control stream, whose virtual writer
			// finishes while the request lives.
			if (version > Version.DRAFT_16)
				this.writer.closed.then(
					() => this.#end(),
					() => this.#end(),
				);
		}
		return this.#ahead;
	}

	// Act on the end as soon as it arrives, even while the message before it waits untaken.
	#readAhead(): Promise<Framed | undefined> {
		const read = this.#readFramed();
		read.then(
			(framed) => {
				if (framed === undefined && finCancels(this.#session.version)) this.#end();
			},
			(err: unknown) => {
				if (err instanceof ProtocolViolation) this.abort(err);
				this.#end();
			},
		);
		return read;
	}

	async #readFramed(): Promise<Framed | undefined> {
		const reader = this.#stream.reader;
		const version = this.#session.version;
		const type = await nextMessage(reader, version, this.#session.client);
		if (type === undefined) return undefined;
		try {
			const size = await reader.u16();
			const framed = new Uint8Array(2 + size);
			framed[0] = size >> 8;
			framed[1] = size & 0xff;
			framed.set(await reader.read(size), 2);
			return { type, reader: new Reader(undefined, framed, version) };
		} catch (err) {
			throw malformed(err, "request stream message");
		}
	}
}

/**
 * A request message the peer sent that does not decode, including one cut short by FIN,
 * breaks the protocol. A reset stream does not.
 * @internal
 */
export function malformed(err: unknown, what: string): unknown {
	if (
		err instanceof ProtocolViolation ||
		err instanceof StreamError ||
		// A transport failure, including a reset whose code the browser did not report.
		(typeof err === "object" &&
			err !== null &&
			"source" in err &&
			(err.source === "session" || err.source === "stream"))
	)
		return err;
	return new ProtocolViolation(`malformed ${what}`, { cause: err });
}
