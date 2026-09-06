import { expect, test } from "bun:test";
import { redact } from "./log.ts";

test("redact drops credentials, query, and fragment", () => {
	expect(redact(new URL("https://user:password@relay.example.com/anon/room?jwt=secret#frag"))).toBe(
		"https://relay.example.com/anon/room",
	);
});

test("redact keeps the port and an empty path", () => {
	expect(redact(new URL("http://localhost:4443?jwt=secret"))).toBe("http://localhost:4443/");
});

test.each([
	[{ DEV: false, MODE: "staging" }, false],
	[{ DEV: true, MODE: "production" }, true],
	[{ NODE_ENV: "production" }, false],
	[{ NODE_ENV: "production", MODE: "staging" }, false],
	[{ MODE: "production" }, false],
	[{ MODE: "development" }, true],
	[undefined, false],
] as const)("diagnostics respect the bundled environment %j", async (env, expected) => {
	const result = await Bun.build({
		entrypoints: [new URL("./log.ts", import.meta.url).pathname],
		target: "browser",
		define: { "import.meta.env": JSON.stringify(env) ?? "undefined" },
	});
	expect(result.success).toBe(true);
	const source = await result.outputs[0].text();
	const { dev } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
	expect(dev()).toBe(expected);
});
