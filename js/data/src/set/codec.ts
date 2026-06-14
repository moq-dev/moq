/**
 * Encodes a set item to and from its wire bytes.
 *
 * Encoding must be deterministic and round-trip: `codec.decode(codec.encode(value))` must equal
 * `value`. Two items are the same set member iff they encode to the same bytes, so distinct items
 * must encode distinctly.
 */
export interface Codec<T> {
	encode(value: T): Uint8Array;
	decode(bytes: Uint8Array): T;
}

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

/** A codec for UTF-8 strings, e.g. a set of track names. */
export const stringCodec: Codec<string> = {
	encode: (value) => textEncoder.encode(value),
	decode: (bytes) => textDecoder.decode(bytes),
};

/** A codec for raw binary items, passed through untouched. */
export const bytesCodec: Codec<Uint8Array> = {
	encode: (value) => value,
	decode: (bytes) => bytes,
};
