# [S] Issue tokens only for usable network roots

## Goal

Rust and JS signers issue tokens only for roots the corresponding network path constructor accepts. Existing signed tokens keep their verification behavior.

## Plan

After the escaped-segment change in [moq-dev/moq#8962](https://github.com/moq-dev/moq/pull/8962), JS signing and verification accepted the root `a\//b` refused by Path.from. Rust auth fixtures also accept that root. Share fixtures for valid escaped separators and invalid empty segments. Check signing, dialing with a valid issued root, and verification of previously signed credentials.
