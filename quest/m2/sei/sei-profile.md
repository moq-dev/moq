# [M] Versioned SEI profile

## Goal

The Hang catalog and resolver define a versioned separated-SEI content
profile. A consumer that does not understand the profile fails before it can
subscribe to video whose in-band SEI has been removed.

## Plan

Define the profile identifier, versioned catalog shape, and content-path
resolution boundary. The legacy unversioned path continues to mean in-band
SEI. The separated profile exposes the top-level `sei` section and a video
rendition only through a resolver that acknowledges that profile; an old
consumer cannot ignore the section and continue with incomplete video.

Land catalog, resolver, Rust, and JavaScript fixtures without changing any
broadcast default or migrating deployed content. Test unknown profile
versions, catalog caches, reconnect, and pre-profile clients failing before
video.

## Required

- [SEI section](/quest/m2/sei/sei.md) - defines the sidecar catalog and
  correlation contract carried by this profile
