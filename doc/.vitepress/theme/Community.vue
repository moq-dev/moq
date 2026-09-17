<script setup>
/**
 * GitHub stars and Discord members in the navbar, with the hand-drawn icons
 * from moq.dev. Hidden below 768px; the stock socialLinks keep GitHub and
 * Discord reachable in the mobile overlay.
 *
 * The counts are fetched in the browser because the site is static and only
 * rebuilt on deploy. Both APIs allow anonymous CORS requests; GitHub's limit is
 * 60/hour per IP, so the result is cached in localStorage for an hour rather
 * than refetched on every page.
 */
import { onMounted, ref } from "vue";

const GITHUB = "https://api.github.com/repos/moq-dev/moq";
const DISCORD = "https://discord.com/api/v10/invites/FCYF3p99mr?with_counts=true";

const CACHE_KEY = "moq.stats";
const CACHE_TTL = 60 * 60 * 1000;

const stars = ref();
const chatters = ref();

function readCache() {
	try {
		const cached = JSON.parse(localStorage.getItem(CACHE_KEY));
		if (cached && Date.now() - cached.at <= CACHE_TTL) return cached;
	} catch {}
}

function writeCache(stats) {
	try {
		localStorage.setItem(CACHE_KEY, JSON.stringify({ ...stats, at: Date.now() }));
	} catch {}
}

async function fetchNumber(url, key) {
	try {
		// Discord echoes the requesting origin in Access-Control-Allow-Origin but
		// marks the response cacheable without `Vary: Origin`, so a response cached
		// for moq.dev fails the CORS check on doc.moq.dev. Skip the HTTP cache;
		// localStorage above is the cache.
		const res = await fetch(url, { cache: "no-store" });
		if (!res.ok) return;
		const value = (await res.json())[key];
		return typeof value === "number" ? value : undefined;
	} catch {}
}

onMounted(async () => {
	let stats = readCache();
	if (!stats) {
		const [s, c] = await Promise.all([
			fetchNumber(GITHUB, "stargazers_count"),
			fetchNumber(DISCORD, "approximate_member_count"),
		]);
		stats = { stars: s, chatters: c };
		if (s !== undefined && c !== undefined) writeCache(stats);
	}
	stars.value = stats.stars;
	chatters.value = stats.chatters;
});
</script>

<template>
	<div class="moq-community">
		<a href="https://github.com/moq-dev/moq" title="GitHub">
			<img src="/emoji/github.svg" alt="GitHub" />
			<span v-if="stars !== undefined">{{ stars.toLocaleString() }}</span>
		</a>
		<a href="https://discord.moq.dev" title="Discord">
			<img src="/emoji/discord.svg" alt="Discord" />
			<span v-if="chatters !== undefined">{{ chatters.toLocaleString() }}</span>
		</a>
	</div>
</template>

<style scoped>
.moq-community {
	display: none;
}

@media (min-width: 768px) {
	.moq-community {
		display: flex;
		align-items: center;
		gap: 1rem;
		margin-left: 1rem;
	}
}

.moq-community a {
	display: flex;
	align-items: center;
	gap: 0.375rem;
	font-size: 0.85rem;
	font-weight: 700;
	color: var(--vp-c-brand-1);
	text-decoration: none;
}

.moq-community img {
	height: 1.75rem;
	width: auto;
	transition: transform 0.25s;
}

.moq-community a:hover img {
	transform: rotate(-6deg);
}
</style>
