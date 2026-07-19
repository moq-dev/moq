/** Decode a chunk unless a fatal decoder error has already closed the WebCodecs decoder. */
export function decodeAudioChunk(decoder: AudioDecoder, chunk: EncodedAudioChunk): boolean {
	if (decoder.state === "closed") return false;
	decoder.decode(chunk);
	return true;
}
