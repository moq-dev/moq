import { expect, test } from "bun:test";
import * as DatagramStream from "./datagram_stream.ts";

test("datagram writer disables send when createWritable throws", () => {
	const transport = {
		datagrams: {
			maxDatagramSize: 1200,
			createWritable: () => {
				throw new Error("unsupported");
			},
		},
	} as unknown as WebTransport;

	expect(DatagramStream.datagramWriter(transport)).toBeUndefined();
});
