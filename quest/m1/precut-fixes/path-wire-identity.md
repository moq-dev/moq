# [S] Preserve broadcast path identity on the JS wire

## Goal

A path accepted by the JS SDK reaches a Rust or JS peer with the same characters. Distinct accepted paths have distinct wire encodings.

## Plan

The lite-06 ANNOUNCE round trip stripped the initial U+FEFF from a concert name. Path.from also admitted a lone UTF-16 surrogate which the writer encoded as U+FFFD. Preserve the leading character on decode and refuse ill-formed strings at creation. Check the existing instance codec and all path entry points. Exercise the actual announcement bytes across implementations.
