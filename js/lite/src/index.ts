const _warned = { value: false };
if (!_warned.value) {
	_warned.value = true;
	console.warn(
		"[@moq/lite] This package has been renamed to @moq/net. The shim re-exports @moq/net and will not receive further updates. Please migrate.",
	);
}

/** @deprecated `@moq/lite` has been renamed to `@moq/net`. This shim re-exports `@moq/net` and will not receive further updates. */
export * from "@moq/net";
