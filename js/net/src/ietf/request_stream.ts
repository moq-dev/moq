import { race } from "@moq/signals";
import { ProtocolViolation, StreamError } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Stream, Writer } from "../stream.ts";
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
export function finCancels(version: IetfVersion): boolean {
	return (
		version === Version.DRAFT_14 ||
		version === Version.DRAFT_15 ||
		version === Version.DRAFT_16 ||
		version === Version.DRAFT_17 ||
		version === Version.DRAFT_18
	);
}

/** Wait for request cancellation, including STOP_SENDING after a clean requester FIN. */
export async function cancelled(stream: Stream, version: IetfVersion): Promise<void> {
	const read = stream.reader.closed.then(() => (finCancels(version) ? undefined : stream.writer.closed));
	// Legacy virtual response writers finish while the adapted control request lives.
	if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16)
		return read.catch(() => undefined);
	await race([read, stream.writer.closed]).catch(() => undefined);
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
	await w.u53(version === Version.DRAFT_14 ? ERROR_14[kind] : RequestError.id);
	await new RequestError({
		requestId: version <= Version.DRAFT_16 ? refusal.requestId : undefined,
		errorCode: toRequestCode(refusal.condition, kind, version),
		reasonPhrase: refusal.reasonPhrase,
	}).encode(w, version);
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

/** Refuse an unrecognized reserved namespace before it reaches the application. */
export async function refuseReserved(stream: Stream, version: IetfVersion): Promise<void> {
	await refuse(stream, version, "subscribe", {
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

/** Wait for a request's FIN or reset while still validating its GOAWAY messages. @internal */
export async function ended(stream: Stream, version: IetfVersion, client: boolean): Promise<void> {
	if (version <= Version.DRAFT_16) return cancelled(stream, version);
	const read = (async () => {
		if ((await nextMessage(stream.reader, version, client)) !== undefined)
			throw new ProtocolViolation("unexpected request stream message");
		if (!finCancels(version)) await stream.writer.closed;
	})();
	await race([read, stream.writer.closed]);
}

/**
 * Read one whole namespace update without abandoning a partial frame.
 * @internal
 */
export async function namespaceUpdate(
	reader: Reader,
	version: IetfVersion,
	client: boolean,
): Promise<{ requestId: bigint; prefix?: Path.Valid; refused?: RequestCondition } | undefined> {
	try {
		// Inside the normalization, so an oversized or truncated type is malformed too.
		const type = await nextMessage(reader, version, client);
		if (type === undefined) return undefined;
		if (type !== 0x02) throw new ProtocolViolation("expected namespace REQUEST_UPDATE");
		return await Message.decode(reader, async (body) => {
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
 * A request message the peer sent that does not decode, including one cut short by FIN,
 * breaks the protocol. A reset stream does not.
 * @internal
 */
export function malformed(err: unknown, what: string): unknown {
	if (
		err instanceof ProtocolViolation ||
		err instanceof StreamError ||
		(typeof err === "object" && err !== null && "source" in err && err.source === "session")
	)
		return err;
	return new ProtocolViolation(`malformed ${what}`, { cause: err });
}
