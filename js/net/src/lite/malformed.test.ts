import { expect, spyOn, test } from "bun:test";
import { SessionCode, StreamCode, StreamError } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Stream, Writer } from "../stream.ts";
import { Connection } from "./connection.ts";
import { Goaway } from "./goaway.ts";
import { ProbeLevel, Setup } from "./setup.ts";
import { DataType, StreamId } from "./stream.ts";
import { ALPN_04, ALPN_05, ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

const versions = [
	[ALPN_05, Version.DRAFT_05],
	[ALPN_06, Version.DRAFT_06],
	[ALPN_07_WIP, Version.DRAFT_07],
] as const;
async function settle(): Promise<void> {
	await new Promise<void>((resolve) => {
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	});
}
function fixture(alpn: string, version: Version, client = true) {
	const pair = createMockTransportPair(alpn);
	const connection = new Connection({ url: new URL("https://relay.example/"), quic: pair.client, version, client });
	const close = spyOn(pair.client, "close");
	return {
		pair,
		connection,
		close,
		violated: () => close.mock.calls.some(([info]) => info?.closeCode === SessionCode.ProtocolViolation),
		async dispose() {
			close.mockRestore();
			connection.abort();
			await settle();
		},
	};
}

for (const [alpn, version] of versions) {
	for (const body of [
		[1, 2, 0],
		[1, 3, 1, 0],
		[1, 3, 1, 1],
		[1, 3, 1, 3],
	]) {
		test(`${alpn}: server SETUP Path/Role presence is refused (${body.join(",")})`, async () => {
			const f = fixture(alpn, version);
			try {
				const writer = await Writer.open(f.pair.server, { version });
				await writer.u53(DataType.Setup);
				await writer.write(new Uint8Array([body.length, ...body]));
				await settle();
				expect(f.violated()).toBe(true);
			} finally {
				await f.dispose();
			}
		});
	}
	test(`${alpn}: a server accepts an unknown client Role as Both`, async () => {
		const f = fixture(alpn, version, false);
		try {
			const writer = await Writer.open(f.pair.server, { version });
			await writer.u53(DataType.Setup);
			await writer.write(new Uint8Array([4, 1, 3, 1, 3]));
			writer.close();
			await settle();
			expect(f.violated()).toBe(false);
		} finally {
			await f.dispose();
		}
	});
	test(`${alpn}: Path is refused on a URI-carrying server binding`, async () => {
		const f = fixture(alpn, version, false);
		try {
			const writer = await Writer.open(f.pair.server, { version });
			await writer.u53(DataType.Setup);
			await writer.write(new Uint8Array([3, 1, 2, 0]));
			await settle();
			expect(f.violated()).toBe(true);
		} finally {
			await f.dispose();
		}
	});
	test(`${alpn}: malformed PROBE reports close the session`, async () => {
		const f = fixture(alpn, version);
		// PROBE opens only while someone observes the estimate.
		const stop = f.connection.probe.subscribe(() => {});
		try {
			const setup = await Writer.open(f.pair.server, { version });
			await setup.u53(DataType.Setup);
			await new Setup({ probe: ProbeLevel.Report }).encode(setup, version);
			setup.close();
			let probe: Stream;
			for (;;) {
				const stream = await Stream.accept(f.pair.server, version);
				if (!stream) throw new Error("PROBE was not opened");
				const type = await stream.reader.u53();
				if (type === StreamId.Probe) {
					probe = stream;
					break;
				}
				expect(type).toBe(StreamId.Auth);
				stream.abort(new StreamError(StreamCode.Cancel));
			}
			// An empty complete report cannot contain its required bitrate and RTT.
			await probe.writer.write(new Uint8Array([0]));
			await settle();
			expect(f.violated()).toBe(true);
		} finally {
			stop();
			await f.dispose();
		}
	});
	test(`${alpn}: a malformed data-stream header remains per-stream`, async () => {
		const f = fixture(alpn, version);
		try {
			const writer = await Writer.open(f.pair.server, { version });
			await writer.u53(DataType.Group);
			await writer.write(new Uint8Array([0]));
			await settle();
			expect(f.violated()).toBe(false);
		} finally {
			await f.dispose();
		}
	});
}

for (const [alpn, version] of [[ALPN_04, Version.DRAFT_04], ...versions] as const) {
	// An unassigned type, and the Tracks and Auth Streams on versions that predate them.
	for (const typ of version === Version.DRAFT_07 ? [9] : [9, StreamId.Tracks, StreamId.Auth]) {
		test(`${alpn}: unknown stream type ${typ} resets only its stream`, async () => {
			const f = fixture(alpn, version);
			try {
				const stream = await Stream.open(f.pair.server, { version });
				await stream.writer.u53(typ);
				await expect(stream.reader.closed).rejects.toThrow();
				await settle();
				expect(f.close).not.toHaveBeenCalled();
			} finally {
				await f.dispose();
			}
		});
	}
	test(`${alpn}: an oversized GOAWAY URI closes the session`, async () => {
		const f = fixture(alpn, version);
		try {
			const stream = await Stream.open(f.pair.server, { version });
			await stream.writer.u53(StreamId.Goaway);
			await new Goaway("a".repeat(8193)).encode(stream.writer, version);
			await settle();
			expect(f.violated()).toBe(true);
		} finally {
			await f.dispose();
		}
	});
	test(`${alpn}: a client cannot redirect a server with GOAWAY`, async () => {
		const f = fixture(alpn, version, false);
		try {
			const stream = await Stream.open(f.pair.server, { version });
			await stream.writer.u53(StreamId.Goaway);
			await new Goaway("https://other.example/").encode(stream.writer, version);
			await settle();
			expect(f.violated()).toBe(true);
		} finally {
			await f.dispose();
		}
	});
	for (const reset of [false, true]) {
		test(`${alpn}: control ${reset ? "reset" : "FIN"} is not a malformed complete message`, async () => {
			const f = fixture(alpn, version);
			try {
				const stream = await Stream.open(f.pair.server, { version });
				await stream.writer.u53(StreamId.Goaway);
				if (reset) stream.abort(new StreamError(StreamCode.Cancel));
				else stream.writer.close();
				await settle();
				expect(f.violated()).toBe(false);
			} finally {
				await f.dispose();
			}
		});
	}
}
