# [S] A configured credential replaces a single empty URL token

## Goal

Rust and JS clients using a configured token can connect to a URL containing a single empty jwt parameter.

## Plan

The dial builders append a second jwt parameter while the service rejects duplicate parameters. Match the service's handling of one empty value when composing the URL. Keep genuine duplicate inputs and nonempty explicit credentials subject to the existing rules. Exercise the built dial URL against the credential parser.
