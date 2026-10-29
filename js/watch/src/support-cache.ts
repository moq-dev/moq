/** Internal key describing which config fields can change a support verdict. */
export const supportCacheKey = Symbol("supportCacheKey");

/** A support probe that can identify the fields its verdict depends on. */
export type CacheableSupported<Config> = ((config: Config) => Promise<boolean>) & {
	[supportCacheKey]?: (config: Config) => string;
};
