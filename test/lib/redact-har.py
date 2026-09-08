#!/usr/bin/env python3
"""Redact cookie objects from a HAR JSON document."""

import json
import sys


def redact_cookie_values(value: object) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if key.lower() == "cookies" and isinstance(child, list):
                for cookie in child:
                    if not isinstance(cookie, dict):
                        continue
                    for field in cookie:
                        if field.lower() == "value":
                            cookie[field] = "<redacted>"
            redact_cookie_values(child)
    elif isinstance(value, list):
        for child in value:
            redact_cookie_values(child)


document = json.load(sys.stdin)
redact_cookie_values(document)
json.dump(document, sys.stdout, separators=(",", ":"))
sys.stdout.write("\n")
