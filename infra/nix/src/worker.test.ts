import { describe, expect, mock, spyOn, test } from "bun:test";

import worker from "./worker";

// Only the Workers runtime has FixedLengthStream; its length check is the
// runtime's, so a pass-through stands in for it here.
Object.assign(globalThis, {
	FixedLengthStream: class extends TransformStream {
		constructor(readonly length: number) {
			super();
		}
	},
});

const key = "00000000000000000000000000000000.narinfo";
const request = (path = key, method = "GET") => new Request(`https://nix.moq.dev/${path}`, { method });
const bucket = (get = mock(async (_key: string): Promise<R2ObjectBody | null> => null)) =>
	({ get }) as unknown as R2Bucket;

describe("bucket reads", () => {
	for (const binding of ["RELEASE", "CI"] as const) {
		test(`${binding} read failure makes GET and HEAD a non-cacheable miss`, async () => {
			const failed = mock(async () => {
				throw new Error("get: whatever the service says");
			});
			const env = { RELEASE: bucket(), CI: bucket(), [binding]: bucket(failed) };
			const log = spyOn(console, "error").mockImplementation(() => {});
			try {
				for (const method of ["GET", "HEAD"]) {
					for (const path of [key, "nar/archive.nar.zst"]) {
						const response = await worker.fetch(request(path, method), env);
						expect(response.status).toBe(404);
						expect(response.headers.get("Cache-Control")).toBe("no-store");
					}
				}
				expect(log).toHaveBeenCalledTimes(4);
				expect(failed).toHaveBeenCalledTimes(4);
				expect(env.RELEASE.get).toHaveBeenCalledTimes(4);
				expect(env.CI.get).toHaveBeenCalledTimes(4);
			} finally {
				log.mockRestore();
			}
		});
	}

	test("a missing binding is a configuration error", async () => {
		await expect(worker.fetch(request(), { RELEASE: undefined, CI: bucket() } as never)).rejects.toThrow(TypeError);
	});

	test("a release miss checks CI and preserves the object's headers and body", async () => {
		const body = "signed metadata";
		const release = mock(async () => null);
		const ci = mock(async () => ({ size: body.length, body: new Response(body).body }) as R2ObjectBody);
		const response = await worker.fetch(request(), { RELEASE: bucket(release), CI: bucket(ci) });
		expect(await response.text()).toBe(body);
		expect(response.headers.get("Content-Type")).toBe("text/x-nix-narinfo");
		expect(response.headers.get("Content-Length")).toBe(String(body.length));
		expect(response.headers.get("Cache-Control")).toBe("public, max-age=300");
		expect(release).toHaveBeenCalledWith(key);
		expect(ci).toHaveBeenCalledWith(key);
	});

	test("a release service error still serves the path from CI", async () => {
		const body = "signed metadata";
		const release = mock(async () => {
			throw new Error("get: Internal error");
		});
		const ci = mock(async () => ({ size: body.length, body: new Response(body).body }) as R2ObjectBody);
		const log = spyOn(console, "error").mockImplementation(() => {});
		try {
			const response = await worker.fetch(request(), { RELEASE: bucket(release), CI: bucket(ci) });
			expect(response.status).toBe(200);
			expect(await response.text()).toBe(body);
			expect(response.headers.get("Cache-Control")).toBe("public, max-age=300");
			expect(log).toHaveBeenCalledTimes(1);
		} finally {
			log.mockRestore();
		}
	});

	test("a plain miss stays cacheable", async () => {
		const response = await worker.fetch(request(), { RELEASE: bucket(), CI: bucket() });
		expect(response.status).toBe(404);
		expect(response.headers.get("Cache-Control")).toBeNull();
	});

	test("cache metadata needs no bucket read", async () => {
		const env = { RELEASE: bucket(), CI: bucket() };
		expect((await worker.fetch(request("nix-cache-info"), env)).status).toBe(200);
		expect(env.RELEASE.get).not.toHaveBeenCalled();
		expect(env.CI.get).not.toHaveBeenCalled();
	});
});

describe("body resume", () => {
	const nar = "nar/archive.nar.zst";
	const bytes = (text: string) => new TextEncoder().encode(text);

	// A body that yields `chunks`, then fails when `fail` is set.
	const body = (chunks: string[], fail = false) =>
		new ReadableStream<Uint8Array>({
			pull(controller) {
				const chunk = chunks.shift();
				if (chunk !== undefined) return controller.enqueue(bytes(chunk));
				if (fail) return controller.error(new Error("read failed"));
				controller.close();
			},
		});
	const object = (stream: ReadableStream<Uint8Array>, size: number, etag = "a") =>
		({ size, etag, body: stream }) as unknown as R2ObjectBody;

	test("a failed body continues from the bytes already sent", async () => {
		const get = mock(async (_key: string, options?: R2GetOptions) =>
			options ? object(body(["cde"]), 3) : object(body(["ab"], true), 5),
		);
		const log = spyOn(console, "error").mockImplementation(() => {});
		try {
			const response = await worker.fetch(request(nar), { RELEASE: bucket(get), CI: bucket() });
			expect(await response.text()).toBe("abcde");
			expect(response.headers.get("Content-Length")).toBe("5");
			expect(get).toHaveBeenCalledTimes(2);
			expect(get.mock.calls[1]).toEqual([nar, { range: { offset: 2 }, onlyIf: { etagMatches: "a" } }]);
		} finally {
			log.mockRestore();
		}
	});

	test("an overwritten object ends the stream instead of splicing bytes", async () => {
		const get = mock(async (_key: string, options?: R2GetOptions) =>
			options ? ({ size: 5, etag: "b" } as unknown as R2Object) : object(body(["ab"], true), 5),
		);
		const log = spyOn(console, "error").mockImplementation(() => {});
		try {
			const response = await worker.fetch(request(nar), { RELEASE: bucket(get), CI: bucket() });
			await expect(response.text()).rejects.toThrow();
		} finally {
			log.mockRestore();
		}
	});

	test("a body that keeps failing stops resuming", async () => {
		const get = mock(async (_key: string, _options?: R2GetOptions) => object(body([], true), 5));
		const log = spyOn(console, "error").mockImplementation(() => {});
		try {
			const response = await worker.fetch(request(nar), { RELEASE: bucket(get), CI: bucket() });
			await expect(response.text()).rejects.toThrow("read failed");
			expect(get).toHaveBeenCalledTimes(6);
		} finally {
			log.mockRestore();
		}
	});

	test("a failed resume read is retried within the same cap", async () => {
		let calls = 0;
		const get = mock(async (_key: string, options?: R2GetOptions) => {
			calls++;
			if (!options) return object(body(["ab"], true), 5);
			if (calls === 2) throw new Error("resume failed");
			return object(body(["cde"]), 3);
		});
		const log = spyOn(console, "error").mockImplementation(() => {});
		try {
			const response = await worker.fetch(request(nar), { RELEASE: bucket(get), CI: bucket() });
			expect(await response.text()).toBe("abcde");
		} finally {
			log.mockRestore();
		}
	});

	test("cancelling the response cancels the object's body", async () => {
		const cancel = mock(() => {});
		const stream = new ReadableStream<Uint8Array>({ pull: (c) => c.enqueue(bytes("ab")), cancel });
		const response = await worker.fetch(request(nar), {
			RELEASE: bucket(mock(async () => object(stream, 100))),
			CI: bucket(),
		});
		await response.body?.cancel();
		expect(cancel).toHaveBeenCalledTimes(1);
	});
});
