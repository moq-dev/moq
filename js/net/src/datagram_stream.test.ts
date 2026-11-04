import { expect, test } from "bun:test";
import * as DatagramStream from "./datagram_stream.ts";

test("a declared createWritable failure is refused without trying the legacy writer", () => {
	const transport = {
		datagrams: {
			maxDatagramSize: 1200,
			createWritable: () => {
				throw new Error("unsupported");
			},
		},
	} as unknown as WebTransport;

	expect(() => DatagramStream.datagramWriter(transport)?.open(0)).toThrow("unsupported");
});
