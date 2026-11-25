import { concat } from "../stream.ts";
import * as Varint from "../varint.ts";
import { lengthBytes } from "./message.ts";
import { type IetfVersion, isDraft23 } from "./version.ts";

/** A control message body behind its Message Length in `version`'s encoding. */
export function framed(body: ArrayLike<number>, version: IetfVersion): Uint8Array {
	return concat([lengthBytes(body.length, version), Uint8Array.from(body)]);
}

/** The body after a control message's Message Length: 16 bits through draft-22, a varint from draft-23. */
export function unframed(bytes: Uint8Array, version: IetfVersion): Uint8Array {
	return isDraft23(version) ? Varint.decodeLeadingOnes(bytes)[1] : bytes.subarray(2);
}
