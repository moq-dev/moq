<script setup>
/**
 * Site-wide notice shown at the top of every page.
 *
 * Two standing caveats for readers:
 * 1. The APIs and protocols are pre-1.0 and still change between releases.
 * 2. Most of these pages are AI generated and may be inaccurate.
 */
</script>

<template>
	<div class="moq-banner">
		<p>
			<strong>Heads up:</strong> MoQ is under active development. The APIs and
			protocols are still evolving and subject to change without notice.
		</p>
		<p>
			Most of this documentation is currently AI generated and may be
			incomplete or inaccurate. Please
			<a href="https://github.com/moq-dev/moq/issues">report anything wrong</a>.
		</p>
	</div>
</template>

<style scoped>
.moq-banner {
	border-bottom: 1px solid var(--vp-c-divider);
	background-color: var(--vp-c-brand-soft);
	padding: 0.5rem 1.5rem;
	text-align: center;
	font-size: 0.85rem;
	line-height: 1.4;
}

.moq-banner p {
	margin: 0.15rem 0;
	color: var(--vp-c-text-2);
}

.moq-banner strong {
	color: var(--vp-c-brand-1);
}
</style>
