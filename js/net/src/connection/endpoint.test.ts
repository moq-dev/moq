import { expect, test } from "bun:test";
import { isLocal, parse, sameHost, show } from "./endpoint.ts";

const local = (url: string) => isLocal(parse(new URL(url)));

// Replays `rs/moq-net/fixtures/endpoints.json`, which moq-net's `is_local` replays too, so the
// two classifiers cannot drift.
const endpoints = (await Bun.file(
	new URL("../../../../rs/moq-net/fixtures/endpoints.json", import.meta.url),
).json()) as {
	url: string;
	local: boolean;
}[];

test("local literals are recognized in every spelling the dial accepts", () => {
	for (const { url, local: expected } of endpoints) expect(local(url), url).toBe(expected);
});

test("a URL with no host is refused, since nothing here dials one", () => {
	expect(() => parse(new URL("unix:///tmp/moq.sock"))).toThrow(TypeError);
	expect(() => parse(new URL("moql:relay.example"))).toThrow(TypeError);
});

test("a raw QUIC URL dials over https at the same host, port, and path", () => {
	const endpoint = parse(new URL("moql://user:pass@127.1:4443/room?jwt=secret"));
	expect(endpoint.scheme).toBe("moql");
	expect(endpoint.host).toEqual({ kind: "ipv4", octets: [127, 0, 0, 1] });
	expect(endpoint.port).toBe(4443);
	expect(endpoint.url.href).toBe("https://user:pass@127.0.0.1:4443/room?jwt=secret");

	const plain = new URL("https://relay.example/room");
	expect(parse(plain).url).toBe(plain);
	expect(parse(new URL("moqt://relay.example")).port).toBe(443);
	expect(parse(new URL("http://relay.example")).port).toBe(80);
});

test("hosts compare as dialed, whatever the scheme or spelling", () => {
	const at = (url: string) => parse(new URL(url));
	expect(sameHost(at("moql://127.1/"), at("https://127.0.0.1:4443/"))).toBe(true);
	expect(sameHost(at("moqt://[::FFFF:7F00:1]/"), at("https://[::ffff:127.0.0.1]/"))).toBe(true);
	expect(sameHost(at("moql://Relay.Example/"), at("wss://relay.example/"))).toBe(true);
	expect(sameHost(at("https://relay.example/"), at("https://relay.example.com/"))).toBe(false);
});

test("an endpoint shows only its scheme, host, and port", () => {
	const shown = (url: string) => show(parse(new URL(url)));
	expect(shown("https://user:password@relay.example.com/anon/room?jwt=secret#frag")).toBe(
		"https://relay.example.com:443",
	);
	expect(shown("moql://relay.example:4443/secret-token/room")).toBe("moql://relay.example:4443");
	expect(shown("http://localhost?jwt=secret")).toBe("http://localhost:80");
});
