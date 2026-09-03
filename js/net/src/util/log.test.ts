import { expect, test } from "bun:test";
import { redact } from "./log.ts";

test("redact drops the query and fragment", () => {
	expect(redact(new URL("https://relay.example.com/anon/room?jwt=secret#frag"))).toBe(
		"https://relay.example.com/anon/room",
	);
});

test("redact keeps the port and an empty path", () => {
	expect(redact(new URL("http://localhost:4443?jwt=secret"))).toBe("http://localhost:4443/");
});
