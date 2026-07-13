import { expect, spyOn, test } from "bun:test";
import { createBandwidth } from "../bandwidth.ts";
import { OriginSchema } from "./origin.ts";
import { Subscriber } from "./subscriber.ts";
import { Version } from "./version.ts";

test("closing the subscriber suppresses probe stream warnings", async () => {
	let readable!: ReadableStreamDefaultController<Uint8Array>;
	const quic = {
		createBidirectionalStream: async () => ({
			readable: new ReadableStream<Uint8Array>({ start: (controller) => (readable = controller) }),
			writable: new WritableStream<Uint8Array>(),
		}),
	} as unknown as WebTransport;
	const subscriber = new Subscriber(quic, Version.DRAFT_03, OriginSchema.parse(1n), createBandwidth());
	const warn = spyOn(console, "warn").mockImplementation(() => {});

	try {
		const probe = subscriber.runProbe();

		await Promise.resolve();
		await Promise.resolve();
		subscriber.close();
		readable.error(new Error("session closed"));
		await probe;

		expect(warn).not.toHaveBeenCalled();
	} finally {
		warn.mockRestore();
	}
});
