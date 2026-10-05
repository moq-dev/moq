# [XS] js/publish audio delay test on mock time

## Goal

The js/publish audio encoder test "a rendition trailing the broadcast's
earliest advertises delay" passes when its file runs alone.

## Plan

It reads the real clock, so timestamps go negative early in the process and
it fails when run alone (#4414). Run it on mock time.
