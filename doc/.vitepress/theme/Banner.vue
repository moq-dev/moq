<script setup>
/**
 * Site-wide notice shown at the top of every page.
 *
 * Reminds readers that MoQ is pre-1.0, so both the APIs/protocols and these
 * (mostly AI-generated) docs are still in flux.
 */
</script>

<template>
	<div class="moq-banner">
		<p>
			<strong>btw:</strong> MoQ is under active development. The APIs and
			protocols are still evolving and will change. Most of this documentation
			is AI generated until things get more stable.
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
