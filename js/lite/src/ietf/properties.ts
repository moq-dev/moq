import type { Reader } from "../stream.ts";
import { type IetfVersion, Version } from "./version.ts";

/// Skip Track Properties from the remaining bytes of a message.
///
/// Track Properties are delta-encoded Key-Value-Pairs (same format as
/// Message Parameters) but with NO count prefix — they extend to the
/// end of the message payload.
///
/// Only present in draft-17+; older drafts don't have Track Properties.
export async function skip(r: Reader, version: IetfVersion): Promise<void> {
	// Track Properties only exist in draft-17+
	if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
		return;
	}

	let prevType = 0n;
	let i = 0;

	while (!(await r.done())) {
		const delta = await r.u62();
		const abs = i === 0 ? delta : prevType + delta;
		prevType = abs;
		i++;

		if (abs % 2n === 0n) {
			// Even type: single varint value
			await r.u62();
		} else {
			// Odd type: length-prefixed bytes
			const len = await r.u53();
			await r.read(len);
		}
	}
}
