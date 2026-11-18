# [M] Fetch the same transcoded group live and from history

## Goal

Historical fills find the source group that produced the requested output group across supported source timescales.

## Plan

In the live mapping from [moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638), source key 1,000,001 on a 1 GHz CMAF track becomes output key 1,000 in microseconds. The inverse used by the historical exact fetch names source key 1,000,000, so a cache miss can report a real group dropped. The audit verified the arithmetic and traced the lookup. Pin the failure through a cold fill, retain or resolve the correct source identity, and check live and historical output for the same key.
