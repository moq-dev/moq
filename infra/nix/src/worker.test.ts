import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";

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

const env = {
	B2_RELEASE_ACCESS_KEY_ID: "release-id",
	B2_RELEASE_SECRET_ACCESS_KEY: "release-secret",
	B2_CI_ACCESS_KEY_ID: "ci-id",
	B2_CI_SECRET_ACCESS_KEY: "ci-secret",
};
const key = "00000000000000000000000000000000.narinfo";
const request = (path = key, method = "GET") => new Request(`https://nix.moq.dev/${path}`, { method });
const bytes = (text: string) => new TextEncoder().encode(text);

// Answers each signed B2 request by bucket, so a test states only what each
// bucket holds. Unlisted buckets miss.
type Handler = (request: Request) => Response | Promise<Response>;
function b2(buckets: { release?: Handler; ci?: Handler }) {
	return spyOn(globalThis, "fetch").mockImplementation((async (input: Request) => {
		const bucket = new URL(input.url).pathname.split("/")[1];
		const handlers: Record<string, Handler | undefined> = {
			"nix-moq-dev": buckets.release,
			"nix-ci-moq-dev": buckets.ci,
		};
		const handler = handlers[bucket];
		return handler ? handler(input) : new Response("NoSuchKey", { status: 404 });
	}) as typeof fetch);
}

const found = (body: BodyInit | null, size: number, etag = '"a"') =>
	new Response(body, { status: 200, headers: { "Content-Length": String(size), ETag: etag } });

afterEach(() => mock.restore());

describe("bucket reads", () => {
	test("each read is a path-style request signed with its bucket's key", async () => {
		const fetch = b2({ ci: () => found("signed metadata", 15) });
		const response = await worker.fetch(request(), env);
		expect(response.status).toBe(200);
		const requests = fetch.mock.calls.map(([input]) => input as Request);
		expect(requests.map((r) => r.url)).toEqual([
			`https://s3.us-east-005.backblazeb2.com/nix-moq-dev/${key}`,
			`https://s3.us-east-005.backblazeb2.com/nix-ci-moq-dev/${key}`,
		]);
		const [release, ci] = requests.map((r) => r.headers.get("Authorization"));
		expect(release).toStartWith("AWS4-HMAC-SHA256 Credential=release-id/");
		expect(ci).toStartWith("AWS4-HMAC-SHA256 Credential=ci-id/");
		expect(ci).toContain("/us-east-005/s3/aws4_request");
	});

	for (const bucket of ["release", "ci"] as const) {
		for (const failure of ["503", "429", "network"]) {
			test(`${bucket} ${failure} failure makes GET and HEAD a non-cacheable miss`, async () => {
				const failed = mock(async (_: Request) => {
					if (failure === "network") throw new Error("connection reset");
					return new Response("busy", { status: Number(failure) });
				});
				b2({ [bucket]: failed });
				const log = spyOn(console, "error").mockImplementation(() => {});
				for (const method of ["GET", "HEAD"]) {
					for (const path of [key, "nar/archive.nar.zst"]) {
						const response = await worker.fetch(request(path, method), env);
						expect(response.status).toBe(404);
						expect(response.headers.get("Cache-Control")).toBe("no-store");
					}
				}
				expect(log).toHaveBeenCalledTimes(4);
				// No retries: each request reads each bucket once.
				expect(failed).toHaveBeenCalledTimes(4);
			});
		}
	}

	test("a rejected key is a configuration error", async () => {
		b2({ release: () => new Response("AccessDenied", { status: 403 }) });
		await expect(worker.fetch(request(), env)).rejects.toThrow("403");
	});

	test("a missing secret is a configuration error", async () => {
		// Even when the release bucket would have served the path.
		b2({ release: () => found("signed metadata", 15) });
		await expect(worker.fetch(request(), { ...env, B2_CI_ACCESS_KEY_ID: undefined } as never)).rejects.toThrow();
	});

	test("a release miss checks CI and preserves the object's headers and body", async () => {
		const body = "signed metadata";
		b2({ ci: () => found(body, body.length) });
		const response = await worker.fetch(request(), env);
		expect(await response.text()).toBe(body);
		expect(response.headers.get("Content-Type")).toBe("text/x-nix-narinfo");
		expect(response.headers.get("Content-Length")).toBe(String(body.length));
		expect(response.headers.get("Cache-Control")).toBe("public, max-age=300");
	});

	test("HEAD asks B2 for headers only", async () => {
		const release = mock((_: Request) => found(null, 42));
		b2({ release });
		const response = await worker.fetch(request(key, "HEAD"), env);
		expect(response.status).toBe(200);
		expect(response.headers.get("Content-Length")).toBe("42");
		expect(release.mock.calls[0][0].method).toBe("HEAD");
	});

	test("a release service error still serves the path from CI", async () => {
		const body = "signed metadata";
		b2({ release: () => new Response("", { status: 500 }), ci: () => found(body, body.length) });
		const log = spyOn(console, "error").mockImplementation(() => {});
		const response = await worker.fetch(request(), env);
		expect(response.status).toBe(200);
		expect(await response.text()).toBe(body);
		expect(response.headers.get("Cache-Control")).toBe("public, max-age=300");
		expect(log).toHaveBeenCalledTimes(1);
	});

	test("a plain miss stays cacheable", async () => {
		b2({});
		const response = await worker.fetch(request(), env);
		expect(response.status).toBe(404);
		expect(response.headers.get("Cache-Control")).toBeNull();
	});

	test("cache metadata needs no bucket read", async () => {
		const fetch = b2({});
		expect((await worker.fetch(request("nix-cache-info"), env)).status).toBe(200);
		expect(fetch).not.toHaveBeenCalled();
	});
});

describe("body resume", () => {
	const nar = "nar/archive.nar.zst";

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
	const partial = (stream: ReadableStream<Uint8Array>) => new Response(stream, { status: 206 });

	test("a failed body continues from the bytes already sent", async () => {
		const release = mock((input: Request) =>
			input.headers.has("Range") ? partial(body(["cde"])) : found(body(["ab"], true), 5),
		);
		b2({ release });
		spyOn(console, "error").mockImplementation(() => {});
		const response = await worker.fetch(request(nar), env);
		expect(await response.text()).toBe("abcde");
		expect(response.headers.get("Content-Length")).toBe("5");
		expect(release).toHaveBeenCalledTimes(2);
		const resumed = release.mock.calls[1][0];
		expect(resumed.headers.get("Range")).toBe("bytes=2-");
		expect(resumed.headers.get("If-Match")).toBe('"a"');
	});

	test("a body that fails after its last byte ends cleanly", async () => {
		const release = mock((_: Request) => found(body(["abcde"], true), 5));
		b2({ release });
		const response = await worker.fetch(request(nar), env);
		expect(await response.text()).toBe("abcde");
		expect(release).toHaveBeenCalledTimes(1);
	});

	test("an overwritten object ends the stream instead of splicing bytes", async () => {
		b2({
			release: (input) =>
				input.headers.has("Range") ? new Response("", { status: 412 }) : found(body(["ab"], true), 5),
		});
		spyOn(console, "error").mockImplementation(() => {});
		const response = await worker.fetch(request(nar), env);
		await expect(response.text()).rejects.toThrow("changed");
	});

	test("a body that keeps failing stops resuming", async () => {
		const release = mock((input: Request) =>
			input.headers.has("Range") ? partial(body([], true)) : found(body([], true), 5),
		);
		b2({ release });
		spyOn(console, "error").mockImplementation(() => {});
		const response = await worker.fetch(request(nar), env);
		await expect(response.text()).rejects.toThrow("read failed");
		expect(release).toHaveBeenCalledTimes(6);
	});

	test("a failed or ignored ranged read is retried within the same cap", async () => {
		let resumes = 0;
		b2({
			release: (input) => {
				if (!input.headers.has("Range")) return found(body(["ab"], true), 5);
				resumes++;
				if (resumes === 1) throw new Error("resume failed");
				// A server ignoring Range would send the whole object again.
				if (resumes === 2) return found(body(["abcde"]), 5);
				return partial(body(["cde"]));
			},
		});
		spyOn(console, "error").mockImplementation(() => {});
		const response = await worker.fetch(request(nar), env);
		expect(await response.text()).toBe("abcde");
		expect(resumes).toBe(3);
	});

	test("cancelling the response cancels the object's body", async () => {
		const cancel = mock(() => {});
		const stream = new ReadableStream<Uint8Array>({ pull: (c) => c.enqueue(bytes("ab")), cancel });
		b2({ release: () => found(stream, 100) });
		const response = await worker.fetch(request(nar), env);
		await response.body?.cancel();
		expect(cancel).toHaveBeenCalledTimes(1);
	});
});
