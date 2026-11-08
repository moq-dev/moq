import { expect, test } from "bun:test";
import { isLocal, parse, sameHost, show } from "./endpoint.ts";

const local = (url: string) => isLocal(parse(new URL(url)));

test("local literals are recognized in every spelling the dial accepts", () => {
	const spellings = [
		"https://127.0.0.1/",
		"https://localhost/",
		"https://localhost./",
		"https://a.localhost/",
		"https://[::1]/",
		"https://[::]/",
		"https://10.0.0.1/",
		"https://172.16.0.1/",
		"https://192.168.1.1/",
		"https://169.254.1.1/",
		"https://0.0.0.0/",
		"https://[::ffff:10.0.0.1]/",
		"https://[fe80::1]/",
		"https://[fc00::1]/",
		"moqt://127.0.0.1/",
		"moqt://[::ffff:127.0.0.1]/",
		// Shorthand, hex, and decimal IPv4, which raw QUIC dials as 127.0.0.1.
		"moql://127.1/",
		"moql://0x7f000001/",
		"moql://2130706433/",
		"moql://0177.0.0.1/",
		"moql://LOCALHOST/",
		"moql://0/",
	];
	for (const url of spellings) expect(local(url), url).toBe(true);

	for (const url of ["https://example.com/", "https://8.8.8.8/", "https://172.32.0.1/", "https://[2606:4700::1]/"]) {
		expect(local(url), url).toBe(false);
	}
	expect(local("moql://8.8.8.8/")).toBe(false);
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
