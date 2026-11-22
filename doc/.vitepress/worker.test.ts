// The worker's content negotiation, against a stand-in for the assets binding.
import { expect, test } from "bun:test";
import worker, { prefersMarkdown } from "../worker";

const files = new Map([
	["/concept.md", "# Concepts"],
	["/setup/install.md", "# Install"],
	["/concept/index.html", "<h1>Concepts</h1>"],
	["/setup/install.html", "<h1>Install</h1>"],
	["/index.html", "<h1>Home</h1>"],
	["/logo.svg", "<svg/>"],
]);

const env = {
	ASSETS: {
		async fetch(request: Request) {
			const { pathname } = new URL(request.url);
			// The assets binding serves /foo from foo.html and /foo/ from foo/index.html.
			const html = pathname.endsWith("/") ? `${pathname}index.html` : `${pathname}.html`;
			if (request.headers.get("If-None-Match") === `"${pathname}"` && files.has(pathname)) {
				return new Response(null, { status: 304 });
			}
			const body = files.get(pathname) ?? files.get(html);
			return body === undefined ? new Response("missing", { status: 404 }) : new Response(body);
		},
	},
};

async function get(path: string, accept?: string) {
	const headers = new Headers();
	if (accept !== undefined) headers.set("Accept", accept);
	const response = await worker.fetch(new Request(`https://doc.moq.dev${path}`, { headers }), env);
	return { response, text: await response.text() };
}

test("markdown wins only when listed and not outranked by HTML", () => {
	expect(prefersMarkdown("text/markdown")).toBe(true);
	expect(prefersMarkdown("text/markdown, text/html;q=0.9")).toBe(true);
	expect(prefersMarkdown("text/markdown;q=0.5, text/html")).toBe(false);
	expect(prefersMarkdown("text/markdown;q=0")).toBe(false);
	expect(prefersMarkdown("text/*, text/markdown;q=0.8")).toBe(false);
	expect(prefersMarkdown("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")).toBe(false);
	expect(prefersMarkdown("*/*")).toBe(false);
	expect(prefersMarkdown("")).toBe(false);
});

test("a page answers Markdown to an agent and HTML to a browser, both varying on Accept", async () => {
	for (const [path, markdown, html] of [
		["/setup/install", "# Install", "<h1>Install</h1>"],
		["/concept/", "# Concepts", "<h1>Concepts</h1>"],
	]) {
		const agent = await get(path, "text/markdown, text/html;q=0.9");
		expect(agent.text).toBe(markdown);
		expect(agent.response.headers.get("Vary")).toBe("Accept");

		const browser = await get(path, "text/html,*/*;q=0.8");
		expect(browser.text).toBe(html);
		expect(browser.response.headers.get("Vary")).toBe("Accept");

		expect((await get(path)).text).toBe(html);
	}
});

test("Markdown is the same file its .md URL serves", async () => {
	const twin = await get("/setup/install.md");
	expect((await get("/setup/install", "text/markdown")).text).toBe(twin.text);
	expect((await get("/setup/install.html", "text/markdown")).text).toBe(twin.text);
});

test("a page without a Markdown copy, or a static file, falls through", async () => {
	expect((await get("/setup/install.md", "text/markdown")).text).toBe("# Install");
	expect((await get("/logo.svg", "text/markdown")).text).toBe("<svg/>");
	expect((await get("/", "text/markdown")).text).toBe("<h1>Home</h1>");
	expect((await get("/nope", "text/markdown")).response.status).toBe(404);
});

test("a conditional request revalidates the Markdown it holds, not the HTML", async () => {
	const response = await worker.fetch(
		new Request("https://doc.moq.dev/setup/install", {
			headers: { Accept: "text/markdown", "If-None-Match": '"/setup/install.md"' },
		}),
		env,
	);
	expect(response.status).toBe(304);
	expect(response.headers.get("Vary")).toBe("Accept");
});

test("a moved page redirects whatever the Accept", async () => {
	const { response } = await get("/setup/linux", "text/markdown");
	expect(response.status).toBe(301);
	expect(response.headers.get("Location")).toBe("https://doc.moq.dev/setup/install");
});
