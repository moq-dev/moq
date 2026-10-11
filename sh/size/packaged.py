"""Identify shipping packages independently of their release version."""
from pathlib import Path


def sizes(root):
    result = {}
    for pattern, kind in [("*.whl", "python"), ("*.aar", "kotlin"), ("*.jar", "kotlin"), ("*.xcframework.zip", "swift")]:
        for path in sorted(Path(root).rglob(pattern)):
            if kind == "python":
                fields = path.stem.split("-")
                if len(fields) not in (5, 6):
                    raise ValueError(f"invalid wheel filename: {path.name}")
                # Distribution, optional build tag and compatibility tags remain meaningful.
                key = "/".join([kind, fields[0], *fields[2:]])
            elif kind == "kotlin":
                version = path.parent.name
                artifact = path.parent.parent.name
                prefix = f"{artifact}-{version}"
                if not path.name.startswith(prefix):
                    raise ValueError(f"expected Maven artifact/version layout: {path}")
                suffix = path.name[len(prefix):]
                if suffix in ("-sources.jar", "-javadoc.jar"):
                    continue
                key = f"{kind}/{artifact}{suffix}"
            else:
                key = f"{kind}/{path.name}"
            key = f"packaged/{key}"
            if key in result:
                raise ValueError(f"duplicate packaged artifact: {key}")
            result[key] = path.stat().st_size
    for kind in ("python", "kotlin", "swift"):
        if not any(key.startswith(f"packaged/{kind}/") for key in result):
            raise ValueError(f"missing packaged {kind} output under {root}")
    return result
