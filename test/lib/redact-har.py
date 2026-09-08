#!/usr/bin/env python3
"""Redact cookie objects from a HAR JSON document."""

import json
import sys


def redact_cookie_values(value: object) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if key.lower() == "cookies":
                if not isinstance(child, list):
                    raise ValueError("HAR cookies field is not an array")
                for cookie in child:
                    if not isinstance(cookie, dict):
                        raise ValueError("HAR cookie entry is not an object")
                    found = False
                    for field in cookie:
                        if field.lower() == "value":
                            cookie[field] = "<redacted>"
                            found = True
                    if not found:
                        raise ValueError("HAR cookie entry has no value")
            redact_cookie_values(child)
    elif isinstance(value, list):
        for child in value:
            redact_cookie_values(child)


document = json.load(sys.stdin)
redact_cookie_values(document)
json.dump(document, sys.stdout, separators=(",", ":"))
sys.stdout.write("\n")
