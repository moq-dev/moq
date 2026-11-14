"""Set up the pinned QUIC Interop Runner and judge its results against cases.toml.

    matrix.py prepare RUNNER IMAGE [--podman]
    matrix.py peers
    matrix.py check RESULTS.json...

`prepare` points the runner at moq-quic's image and the pinned peers and
simulator, and `peers` lists the peers. `check` fails on every cell whose result differs from the one
cases.toml expects, including a known failure that now passes.
"""

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

CONFIG = Path(__file__).with_name("cases.toml")
SELF = "moq-quic"


def load(path=CONFIG):
    with open(path, "rb") as f:
        config = tomllib.load(f)
    unsupported = config.get("unsupported", {})
    for role in unsupported:
        if role not in ("client", "server"):
            sys.exit(f"cases.toml: unsupported.{role} is not a role")
    for known in config.get("known", []):
        missing = {"client", "server", "case", "result", "why"} - known.keys()
        if missing:
            sys.exit(f"cases.toml: known entry {known} lacks {sorted(missing)}")
        if SELF not in (known["client"], known["server"]):
            sys.exit(f"cases.toml: known entry {known} does not involve {SELF}")
    return config


def prepare(args):
    config = load()
    runner = Path(args.runner)
    impls = {SELF: {"image": args.image, "url": "https://github.com/moq-dev/moq", "role": "both"}}
    for name, peer in config["peers"].items():
        impls[name] = {"image": peer["image"], "url": peer["url"], "role": "both"}
    (runner / "implementations_quic.json").write_text(json.dumps(impls, indent=2) + "\n")

    compose = runner / "docker-compose.yml"
    text = compose.read_text()
    pins = {
        "martenseemann/quic-network-simulator": config["runner"]["sim"],
        "martenseemann/quic-interop-iperf-endpoint": config["runner"]["iperf"],
    }
    for image, pinned in pins.items():
        text, n = re.subn(rf"^(\s+image: ){re.escape(image)}$", rf"\g<1>{pinned}", text, flags=re.M)
        if n == 0:
            sys.exit(f"docker-compose.yml no longer names {image}; update the pins")
    if args.podman:
        # Rootless Podman refuses interface names (a Docker 28.1 field) and a memlock
        # limit above its own, and SELinux hides the runner's bind mounts from a
        # labelled container. Without names, the simulator's networks still attach in
        # the order eth0, eth1 that it expects.
        text = re.sub(r"^\s+interface_name: .*\n", "", text, flags=re.M)
        text = re.sub(r"^\s+ulimits:\n\s+memlock: .*\n", "", text, flags=re.M)
        text = re.sub(
            r"^(\s+)container_name: ", r"\1security_opt:\n\1  - label=disable\n\1container_name: ", text, flags=re.M
        )
    compose.write_text(text)


def expected(config, client, server, case):
    """The result a cell must read, and why it is not a pass."""
    for known in config.get("known", []):
        if (known["client"], known["server"], known["case"]) == (client, server, case):
            return known["result"], known["why"]
    unsupported = config.get("unsupported", {})
    for role, name in (("client", client), ("server", server)):
        if name == SELF and case in unsupported.get(role, {}):
            return "unsupported", unsupported[role][case]
    return "succeeded", ""


def cells(results):
    """Yield (client, server, case, result) from one runner JSON export."""
    servers = results["servers"]
    for i, client in enumerate(results["clients"]):
        for j, server in enumerate(servers):
            row = i * len(servers) + j
            for cell in results["results"][row] + results["measurements"][row]:
                yield client, server, cell["name"], cell["result"]


def check(args):
    config = load()
    mismatches = []
    seen = 0
    for path in args.results:
        with open(path) as f:
            results = json.load(f)
        for client, server, case, result in cells(results):
            seen += 1
            want, why = expected(config, client, server, case)
            if result != want:
                mismatches.append((client, server, case, result, want, why))
    if seen == 0:
        sys.exit("no results: the runner ran no cell")
    for client, server, case, result, want, why in mismatches:
        note = f" ({why})" if why else ""
        print(f"MISMATCH client={client} server={server} {case}: got {result}, expected {want}{note}")
    print(f"{seen - len(mismatches)}/{seen} cells as expected")
    return 1 if mismatches else 0


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("runner")
    p.add_argument("image")
    p.add_argument("--podman", action="store_true")
    c = sub.add_parser("check")
    c.add_argument("results", nargs="+")
    sub.add_parser("peers")
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args)
        return 0
    if args.command == "peers":
        print(",".join(load()["peers"]))
        return 0
    return check(args)


if __name__ == "__main__":
    sys.exit(main())
