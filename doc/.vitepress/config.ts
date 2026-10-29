import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { type DefaultTheme, defineConfig } from "vitepress";
import llmstxt from "vitepress-plugin-llms";
import { syncDrafts } from "./drafts";

// Generated before VitePress enumerates routes, so the pages and this sidebar
// list stay in sync with the kramdown-rfc sources under drafts/.
const drafts = syncDrafts();

// The JS API reference under lib/js/api/, generated the same way from the
// packages' TSDoc. TypeDoc runs in its own process, since it holds the whole
// program in memory and needs its own TypeScript. A build always regenerates
// it; the dev server reuses the last one until a package changes.
const generator = fileURLToPath(new URL("./api/index.ts", import.meta.url));
const build = process.argv.slice(2).includes("build");
execFileSync("bun", build ? [generator] : [generator, "--cached"], { stdio: "inherit" });
const reference = (items: DefaultTheme.SidebarItem[], parent?: string): DefaultTheme.SidebarItem[] =>
	items
		// A package with several entrypoints lists its main one again under itself.
		.filter((item) => item.link !== parent)
		.map((item) => {
			const link = item.link?.replace(/\.md$/, "");
			return { ...item, link, items: item.items && reference(item.items, item.link) };
		});
const api = reference(JSON.parse(readFileSync(new URL("../lib/js/api/typedoc-sidebar.json", import.meta.url), "utf8")));

export default defineConfig({
	title: "Media over QUIC",
	description: "Real-time latency at massive scale",
	base: "/",

	head: [
		["link", { rel: "icon", href: "/favicon.svg", type: "image/svg+xml" }],
		["meta", { property: "og:type", content: "website" }],
		["meta", { property: "og:title", content: "Media over QUIC" }],
		[
			"meta",
			{
				property: "og:description",
				content: "Real-time latency at massive scale",
			},
		],
		["meta", { property: "og:image", content: "https://doc.moq.dev/og.png" }],
		["meta", { property: "og:image:width", content: "2400" }],
		["meta", { property: "og:image:height", content: "1350" }],
		["meta", { property: "og:url", content: "https://doc.moq.dev" }],
		["meta", { property: "og:site_name", content: "Media over QUIC" }],
		["meta", { name: "twitter:card", content: "summary_large_image" }],
		["meta", { name: "twitter:title", content: "Media over QUIC" }],
		[
			"meta",
			{
				name: "twitter:description",
				content: "Real-time latency at massive scale",
			},
		],
		["meta", { name: "twitter:image", content: "https://doc.moq.dev/og.png" }],
		["meta", { name: "theme-color", content: "#0f172a" }],
	],

	appearance: "force-dark",

	themeConfig: {
		logo: "/favicon.svg",

		nav: [
			{ text: "Setup", link: "/setup/" },
			{ text: "Concepts", link: "/concept/" },
			{ text: "Apps", link: "/bin/" },
			{
				text: "Libraries",
				link: "/lib/",
				items: [
					{ text: "Choose a library", link: "/lib/" },
					{ text: "Examples", link: "/setup/examples" },
					{ text: "Rust", link: "/lib/rs/" },
					{ text: "TypeScript", link: "/lib/js/" },
					{ text: "Swift", link: "/lib/swift/" },
					{ text: "Kotlin", link: "/lib/kt/" },
					{ text: "Python", link: "/lib/py/" },
					{ text: "Go", link: "/lib/go/" },
					{ text: "Dart", link: "/lib/dart/" },
					{ text: "C++", link: "/lib/cpp/" },
					{ text: "C", link: "/lib/c/" },
				],
			},
			{ text: "Drafts", link: "/draft/" },
		],

		sidebar: {
			"/setup/": [
				{
					text: "Setup",
					link: "/setup/",
					items: [
						{ text: "Quick start", link: "/setup/" },
						{ text: "Examples", link: "/setup/examples" },
						{ text: "Install", link: "/setup/install" },
						{ text: "Development", link: "/setup/dev" },
						{ text: "Repository map", link: "/setup/repository" },
						{ text: "Troubleshooting", link: "/setup/troubleshooting" },
						{ text: "Production", link: "/setup/prod" },
						{ text: "Redundant ingest", link: "/setup/redundancy" },
						{ text: "Upgrade", link: "/setup/upgrade" },
						{ text: "Coding agents", link: "/setup/agent" },
					],
				},
			],

			"/concept/": [
				{
					text: "Concepts",
					link: "/concept/",
					items: [
						{ text: "Overview", link: "/concept/" },
						{ text: "Transport", link: "/concept/transport" },
						{ text: "moq-lite", link: "/concept/moq-lite" },
						{ text: "hang", link: "/concept/hang" },
						{ text: "Audio jitter", link: "/concept/audio-jitter" },
						{ text: "Compressed tracks", link: "/concept/compression" },
						{ text: "Stats", link: "/concept/stats" },
						{ text: "Standards", link: "/concept/standard" },
						{
							text: "Use cases",
							link: "/concept/use-case/",
							items: [
								{ text: "vs HLS/DASH", link: "/concept/use-case/distribution" },
								{ text: "vs RTMP/SRT", link: "/concept/use-case/contribution" },
								{ text: "vs WebRTC", link: "/concept/use-case/conferencing" },
								{ text: "For AI", link: "/concept/use-case/ai" },
								{ text: "For teleoperation", link: "/concept/use-case/teleoperation" },
							],
						},
					],
				},
			],

			"/draft/": [
				{
					text: "Internet-Drafts",
					link: "/draft/",
					items: drafts.map((draft) => ({ text: draft.title, link: draft.link })),
				},
			],

			"/bin/": [
				{
					text: "Applications",
					link: "/bin/",
					items: [
						{
							text: "moq-relay",
							link: "/bin/relay/",
							items: [
								{ text: "Configuration", link: "/bin/relay/config" },
								{ text: "Authentication", link: "/bin/relay/auth" },
								{ text: "Clustering", link: "/bin/relay/cluster" },
								{ text: "HTTP", link: "/bin/relay/http" },
								{ text: "Deployment", link: "/setup/prod" },
							],
						},
						{
							text: "moq-cli",
							link: "/bin/cli",
							items: [{ text: "Inspect a relay", link: "/bin/inspect" }],
						},
						{ text: "moq-studio", link: "/bin/studio" },
						{
							text: "Gateways",
							items: [
								{ text: "RTMP", link: "/bin/rtmp" },
								{ text: "SRT", link: "/bin/srt" },
								{ text: "WebRTC", link: "/bin/rtc" },
								{ text: "HLS", link: "/bin/hls" },
							],
						},
						{
							text: "Plugins",
							items: [
								{ text: "OBS Studio", link: "/bin/obs" },
								{ text: "GStreamer", link: "/bin/gstreamer" },
							],
						},
						{ text: "Demos", link: "/bin/demo" },
					],
				},
			],

			"/lib/": [
				{
					text: "Libraries",
					link: "/lib/",
					items: [
						{ text: "Choose a library", link: "/lib/" },
						{ text: "Examples", link: "/setup/examples" },
						{
							text: "Rust",
							link: "/lib/rs/",
							items: [
								{ text: "moq-net", link: "/lib/rs/moq-net" },
								{ text: "hang", link: "/lib/rs/hang" },
								{ text: "moq-mux", link: "/lib/rs/moq-mux" },
								{ text: "moq-video", link: "/lib/rs/moq-video" },
								{ text: "moq-audio", link: "/lib/rs/moq-audio" },
								{ text: "moq-auth", link: "/lib/rs/moq-auth" },
								{ text: "moq-room", link: "/lib/rs/moq-room" },
								{ text: "moq-json", link: "/lib/rs/moq-json" },
								{ text: "moq-clock", link: "/lib/rs/moq-clock" },
							],
						},
						{
							text: "TypeScript",
							link: "/lib/js/",
							items: [
								{ text: "@moq/net", link: "/lib/js/net" },
								{ text: "@moq/hang", link: "/lib/js/hang" },
								{ text: "@moq/watch", link: "/lib/js/watch" },
								{ text: "@moq/publish", link: "/lib/js/publish" },
								{ text: "@moq/video", link: "/lib/js/video" },
								{ text: "@moq/room", link: "/lib/js/room" },
								{ text: "@moq/auth", link: "/lib/js/auth" },
								{ text: "@moq/signals", link: "/lib/js/signals" },
								{ text: "@moq/clock", link: "/lib/js/clock" },
								{ text: "@moq/json", link: "/lib/js/json" },
								{ text: "API reference", link: "/lib/js/api/" },
							],
						},
						{ text: "Swift", link: "/lib/swift/" },
						{ text: "Kotlin", link: "/lib/kt/" },
						{ text: "Python", link: "/lib/py/" },
						{ text: "Go", link: "/lib/go/" },
						{ text: "Dart", link: "/lib/dart/" },
						{ text: "C++", link: "/lib/cpp/" },
						{ text: "C", link: "/lib/c/" },
						{ text: "Layers", link: "/lib/layers" },
					],
				},
			],

			"/lib/js/api/": [
				{ text: "TypeScript", link: "/lib/js/" },
				{ text: "API reference", link: "/lib/js/api/", items: api },
			],
		},

		// GitHub and Discord stay here so the mobile overlay still has them.
		// theme/Community.vue is the desktop pair, with live counts and the
		// hand-drawn icons from moq.dev; custom.css hides these two in the
		// navbar so they do not duplicate.
		socialLinks: [
			{ icon: "github", link: "https://github.com/moq-dev/moq" },
			{ icon: "discord", link: "https://discord.gg/FCYF3p99mr" },
			{ icon: "x", link: "https://x.com/kixelated" },
			{ icon: "bluesky", link: "https://bsky.app/profile/kixel.me" },
			{ icon: "linkedin", link: "https://www.linkedin.com/in/luke-curley-635a457/" },
		],

		editLink: {
			// /draft/ pages are generated, so edits go to the kramdown-rfc source
			// instead. VitePress ships this function to the browser by stringifying
			// it, so it has to stay closure-free: no imports, no outer variables.
			// The inverse of `slug()` in drafts.ts.
			pattern: ({ filePath }) => {
				const generated = filePath.startsWith("draft/") && filePath !== "draft/index.md";
				const slug = filePath.slice("draft/".length);
				const path = generated
					? `drafts/${slug.startsWith("draft-") ? slug : `draft-lcurley-${slug}`}`
					: `doc/${filePath}`;
				return `https://github.com/moq-dev/moq/edit/main/${path}`;
			},
			text: "Edit this page on GitHub",
		},

		search: {
			provider: "local",
		},

		lastUpdated: {
			text: "Last updated",
		},

		footer: {
			message: "Licensed under MIT or Apache-2.0",
			copyright: "Copyright © 2026-present moq.dev",
		},
	},

	markdown: {
		theme: "github-dark",
		lineNumbers: true,
	},

	// Serves /llms.txt (an index), /llms-full.txt (every page inline), and a
	// .md twin of every page so agents can read the docs without the HTML.
	vite: {
		plugins: [
			llmstxt({
				domain: "https://doc.moq.dev",
				// Static files are already served verbatim.
				ignoreFiles: ["public/**"],
				// llms.txt still indexes the API reference and each page has its .md twin,
				// but inlining its 1.2 MB of signatures would more than double llms-full.txt.
				ignoreFilesPerOutput: { llmsFullTxt: ["lib/js/api/**"] },
			}),
		],
	},

	// Agent instructions, not a page.
	srcExclude: ["AGENTS.md"],

	ignoreDeadLinks: [
		// Localhost URLs are intentional for development examples and aren't
		// reachable at build time (e.g. the relay on :4443, the dev server on :5173).
		/^https?:\/\/localhost(:\d+)?(\/|\?|#|$)/,
	],
});
