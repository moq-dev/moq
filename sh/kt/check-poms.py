#!/usr/bin/env python3
"""Refuse unpublished development dependencies in generated wrapper POMs."""

from pathlib import Path
import sys
import xml.etree.ElementTree as ET

namespace = {"m": "http://maven.apache.org/POM/4.0.0"}
for argument in sys.argv[1:]:
    directory = Path(argument)
    poms = sorted([*directory.rglob("*.pom"), *directory.rglob("pom-default.xml")])
    if not poms:
        raise SystemExit(f"No publication POMs in {directory}")
    for pom in poms:
        for dependency in ET.parse(pom).findall("m:dependencies/m:dependency", namespace):
            group = dependency.findtext("m:groupId", namespaces=namespace)
            version = dependency.findtext("m:version", namespaces=namespace)
            if group == "dev.moq" and version == "0.0.0-dev":
                raise SystemExit(f"Unpublished development dependency in {pom}")
