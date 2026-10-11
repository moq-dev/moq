import { race } from "@moq/signals";
import type { Stream } from "../stream.ts";
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
