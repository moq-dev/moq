import { race } from "@moq/signals";
import { ProtocolViolation, StreamError } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Stream } from "../stream.ts";
import { type RequestCondition, toRequestCode } from "./error.ts";
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
 * Read one whole namespace update without abandoning a partial frame.
 * @internal
 */
export async function namespaceUpdate(
	reader: Reader,
	version: IetfVersion,
): Promise<{ requestId: bigint; prefix?: Path.Valid; refused?: RequestCondition } | undefined> {
	if (await reader.done()) return undefined;
	try {
		// Inside the normalization, so an oversized or truncated type is malformed too.
		if ((await reader.u53()) !== 0x02) throw new ProtocolViolation("expected namespace REQUEST_UPDATE");
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
		if (err instanceof ProtocolViolation || err instanceof StreamError) throw err;
		throw new ProtocolViolation("malformed namespace REQUEST_UPDATE", { cause: err });
	}
}
