import { expect, mock, test } from "bun:test";

import worker from "./worker";

const objects: Record<string, string> = {
	"moq.dev/moq-ffi/@v/list": "v0.4.0\nv0.4.1\n",
	"moq.dev/moq-ffi/@v/v0.4.1.info": '{"Version":"v0.4.1","Time":"2026-11-12T00:00:00Z"}',
	"moq.dev/moq-ffi/@v/v0.4.1.mod": "module moq.dev/moq-ffi\n",
	"moq.dev/moq-ffi/@v/v0.4.1.zip": "PK",
	// Not a GOPROXY path, so never served even though the bucket holds it.
	"moq.dev/moq-ffi/notes.txt": "private",
};

const env = () => ({
	GO: {
		get: mock(async (key: string) => {
			const body = objects[key];
			return body === undefined ? null : { size: body.length, body };
		}),
	} as unknown as R2Bucket,
});

const fetch = (path: string, method = "GET", e = env()) =>
	worker.fetch(new Request(`https://go.moq.dev/${path}`, { method }), e);

test("serves each GOPROXY file with its type and cache lifetime", async () => {
	for (const [file, type, cache] of [
		["list", "text/plain; charset=utf-8", "public, max-age=60"],
		["v0.4.1.info", "application/json", "public, max-age=31536000, immutable"],
		["v0.4.1.mod", "text/plain; charset=utf-8", "public, max-age=31536000, immutable"],
		["v0.4.1.zip", "application/zip", "public, max-age=31536000, immutable"],
	]) {
		const key = `moq.dev/moq-ffi/@v/${file}`;
		const response = await fetch(key);
		expect(response.status).toBe(200);
		expect(response.headers.get("Content-Type")).toBe(type);
		expect(response.headers.get("Cache-Control")).toBe(cache);
		expect(response.headers.get("Content-Length")).toBe(objects[key].length.toString());
		expect(await response.text()).toBe(objects[key]);
	}
});

test("HEAD returns the headers without a body", async () => {
	const response = await fetch("moq.dev/moq-ffi/@v/v0.4.1.zip", "HEAD");
	expect(response.status).toBe(200);
	expect(response.headers.get("Content-Length")).toBe("2");
	expect(await response.text()).toBe("");
});

test("a missing version, @latest, or a non-GOPROXY key is an uncached 404", async () => {
	for (const path of [
		"moq.dev/moq-ffi/@v/v9.9.9.info",
		"moq.dev/moq-ffi/@latest",
		"moq.dev/moq-ffi/notes.txt",
		"moq.dev/moq-ffi/@v/sub/v0.4.1.zip",
		"moq.dev/moq-ffi/@v/v0.4.1.tar",
	]) {
		const e = env();
		const response = await fetch(path, "GET", e);
		expect(response.status).toBe(404);
		expect(response.headers.get("Cache-Control")).toBe("no-store");
	}
});

test("only a GOPROXY path reads the bucket", async () => {
	const e = env();
	await fetch("moq.dev/moq-ffi/notes.txt", "GET", e);
	expect(e.GO.get).not.toHaveBeenCalled();
});

test("refuses writes", async () => {
	const response = await fetch("moq.dev/moq-ffi/@v/list", "PUT");
	expect(response.status).toBe(405);
});
