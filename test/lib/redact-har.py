#!/usr/bin/env python3
"""Redact credential-bearing objects from a HAR JSON document."""

import json
import sys


SENSITIVE_PARAMETERS = {"jwt", "token", "access_token", "auth", "key", "secret"}


def redact_entries(entries: object, field: str, redact_all: bool) -> None:
    if not isinstance(entries, list):
        raise ValueError(f"HAR {field} field is not an array")
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError(f"HAR {field} entry is not an object")
        names = [key for key in entry if key.lower() == "name"]
        values = [key for key in entry if key.lower() == "value"]
        if not names or not values:
            raise ValueError(f"HAR {field} entry has no name/value pair")
        name = entry[names[0]]
        if not isinstance(name, str):
            raise ValueError(f"HAR {field} entry name is not a string")
        if redact_all or name.lower() in SENSITIVE_PARAMETERS:
            for key in values:
                entry[key] = "<redacted>"


def redact_values(value: object) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            field = key.lower()
            if field == "cookies":
                redact_entries(child, "cookies", True)
            elif field == "querystring":
                redact_entries(child, field, False)
            elif field == "postdata":
                if not isinstance(child, dict):
                    raise ValueError("HAR postData field is not an object")
                for post_key, post_value in child.items():
                    if post_key.lower() == "params":
                        redact_entries(post_value, "postData params", False)
            redact_values(child)
    elif isinstance(value, list):
        for child in value:
            redact_values(child)


document = json.load(sys.stdin)
redact_values(document)
json.dump(document, sys.stdout, separators=(",", ":"))
sys.stdout.write("\n")
