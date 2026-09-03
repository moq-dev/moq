<script setup>
/**
 * Site-wide notice shown at the top of every page.
 *
 * Reminds readers that MoQ is pre-1.0 and its public surfaces may change.
 */
</script>

<template>
	<div class="moq-banner">
		<p>
			<strong>MoQ is under active development.</strong> APIs and protocols may
			change between releases.
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
