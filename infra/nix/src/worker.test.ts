import { describe, expect, mock, spyOn, test } from "bun:test";

import worker from "./worker";

const key = "00000000000000000000000000000000.narinfo";
const request = (path = key, method = "GET") => new Request(`https://nix.moq.dev/${path}`, { method });
const bucket = (get = mock(async (_key: string): Promise<R2ObjectBody | null> => null)) =>
	({ get }) as unknown as R2Bucket;

describe("bucket reads", () => {
	for (const binding of ["RELEASE", "CI"] as const) {
		for (const code of [10001, 10043]) {
			test(`${binding} service error ${code} makes GET and HEAD a non-cacheable miss`, async () => {
				const error = new Error(`get: Service failed (${code})`);
				const failed = mock(async () => {
					throw error;
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

		for (const error of [
			new Error("get: Unauthorized (10002)"),
			new Error("get: Access denied (10003)"),
			new Error("get: No such bucket (10006)"),
			new Error("get: Invalid object name (10020)"),
			new Error("get: Unknown failure (0)"),
			new Error("unrelated 10001"),
			new TypeError("bad binding"),
		]) {
			test(`${binding} preserves ${error}`, async () => {
				const env = {
					RELEASE: bucket(),
					CI: bucket(),
					[binding]: bucket(mock(async () => Promise.reject(error))),
				};
				await expect(worker.fetch(request(), env)).rejects.toBe(error);
			});
		}
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
			throw new Error("get: Internal error (10001)");
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
