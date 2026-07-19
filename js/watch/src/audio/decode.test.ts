import { expect, mock, test } from "bun:test";
import { decodeAudioChunk } from "./decode";

test("does not decode another chunk after a fatal decoder error", () => {
	const decode = mock(() => {});
	const decoder = { state: "closed", decode } as unknown as AudioDecoder;
	const chunk = {} as EncodedAudioChunk;

	expect(decodeAudioChunk(decoder, chunk)).toBeFalse();
	expect(decode).not.toHaveBeenCalled();
});

test("decodes a chunk while the decoder is configured", () => {
	const decode = mock(() => {});
	const decoder = { state: "configured", decode } as unknown as AudioDecoder;
	const chunk = {} as EncodedAudioChunk;

	expect(decodeAudioChunk(decoder, chunk)).toBeTrue();
	expect(decode).toHaveBeenCalledWith(chunk);
});
