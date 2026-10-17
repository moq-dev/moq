import { race } from "@moq/signals";
import { ProtocolViolation, StreamError } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Stream } from "../stream.ts";
import { type RequestCondition, toRequestCode } from "./error.ts";
import { GoAway } from "./goaway.ts";
import * as Message from "./message.ts";
import { Parameters } from "./parameters.ts";
import { RequestError } from "./request.ts";
import { MalformedAuthToken } from "./token.ts";
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

/** Refuse an unrecognized reserved namespace before it reaches the application. */
export async function refuseReserved(stream: Stream, version: IetfVersion): Promise<void> {
	await stream.writer.u53(RequestError.id);
	await new RequestError({
		requestId: undefined,
		errorCode: toRequestCode("does_not_exist", "subscribe", version),
		reasonPhrase: "reserved namespace does not exist",
	}).encode(stream.writer, version);
	stream.close();
}

/**
 * The GOAWAYs the peer sent on one request stream. `client` is our own role: only a server
 * may name a new session.
 * @internal
 */
export type GoAways = { client: boolean; seen: boolean };

/**
 * Read the type of a request stream's next message, or undefined once the peer finishes.
 * From draft 18 on, the peer may send one GOAWAY on a request stream, consumed here; a second,
 * or a client's naming a new session, is a protocol violation.
 * @internal
 */
export async function nextMessage(reader: Reader, version: IetfVersion, goaway: GoAways): Promise<number | undefined> {
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
}

/**
 * Read one whole namespace update without abandoning a partial frame.
 * @internal
 */
export async function namespaceUpdate(
	reader: Reader,
	version: IetfVersion,
	goaway: GoAways,
): Promise<{ requestId: bigint; prefix?: Path.Valid; refused?: RequestCondition } | undefined> {
	try {
		// Inside the normalization, so an oversized or truncated type is malformed too.
		const type = await nextMessage(reader, version, goaway);
		if (type === undefined) return undefined;
		if (type !== 0x02) throw new ProtocolViolation("expected namespace REQUEST_UPDATE");
		return await Message.decode(reader, async (body) => {
			const requestId = await body.u62();
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
	if (err instanceof ProtocolViolation || err instanceof StreamError) return err;
	return new ProtocolViolation(`malformed ${what}`, { cause: err });
}
