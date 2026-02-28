#!/usr/bin/env python3
"""Alert when any systemd service breaches its cgroup memory limits.

Uses poll() on cgroup v2 memory.events files to get instant notification
when high/max/oom/oom_kill counters change.

Usage:
    ./memory-alert.py <service> [<service> ...] [--webhook URL]

Environment variables:
    MEMORY_ALERT_WEBHOOK - webhook URL (overridden by --webhook)
"""

import argparse
import json
import os
import select
import socket
import subprocess
import sys
import urllib.request

CGROUP_BASE = "/sys/fs/cgroup"
SLICE_PATH = f"{CGROUP_BASE}/system.slice"
COUNTERS = ("high", "max", "oom", "oom_kill")


def get_cgroup_path(service):
    """Get the cgroup path for a systemd service."""
    result = subprocess.run(
        ["systemctl", "show", service, "-p", "ControlGroup", "--value"],
        capture_output=True, text=True, check=True,
    )
    cgroup = result.stdout.strip()
    if not cgroup:
        raise RuntimeError(f"{service}: no cgroup found (is it running?)")
    return f"{CGROUP_BASE}{cgroup}"


def discover_services():
    """Find all services with memory limits (MemoryHigh or MemoryMax set)."""
    services = []
    for entry in os.listdir(SLICE_PATH):
        if not entry.endswith(".service"):
            continue
        cgroup = f"{SLICE_PATH}/{entry}"
        for limit in ("memory.high", "memory.max"):
            path = f"{cgroup}/{limit}"
            try:
                with open(path) as f:
                    val = f.read().strip()
                if val != "max":
                    services.append(entry)
                    break
            except OSError:
                continue
    return services


def read_events(path):
    """Parse memory.events into a dict of counters."""
    counters = {}
    with open(path) as f:
        for line in f:
            parts = line.split()
            if len(parts) == 2 and parts[0] in COUNTERS:
                counters[parts[0]] = int(parts[1])
    return counters


def read_current_mb(cgroup_path):
    """Read current memory usage in MB."""
    try:
        with open(f"{cgroup_path}/memory.current") as f:
            return int(f.read().strip()) // (1024 * 1024)
    except (OSError, ValueError):
        return 0


def send_webhook(url, message):
    """Post a message to a Discord/Slack webhook."""
    payload = json.dumps({"content": message}).encode()
    req = urllib.request.Request(
        url, data=payload,
        headers={"Content-Type": "application/json"},
    )
    try:
        urllib.request.urlopen(req, timeout=10)
    except Exception as e:
        print(f"Warning: webhook failed: {e}", file=sys.stderr)


def alert(service, current_mb, changes, webhook_url):
    """Format and send an alert."""
    hostname = socket.gethostname()
    lines = [f"Memory alert on {hostname} for {service} ({current_mb}MB):"]
    for counter, (old, new) in changes.items():
        lines.append(f"  {counter}: {old} -> {new}")

    msg = "\n".join(lines)
    print(msg)

    if webhook_url:
        send_webhook(webhook_url, msg)


REDISCOVER_MS = 60_000


def add_watch(poller, watches, service):
    """Add a service to the poll watch set."""
    cgroup_path = get_cgroup_path(service)
    events_path = f"{cgroup_path}/memory.events"

    f = open(events_path, "r")
    fd = f.fileno()
    poller.register(fd, select.POLLPRI | select.POLLERR)
    watches[fd] = (service, f, cgroup_path, read_events(events_path))

    print(f"Watching {service}")


def remove_watch(poller, watches, service):
    """Remove a service from the poll watch set."""
    for fd, (svc, f, _, _) in list(watches.items()):
        if svc == service:
            poller.unregister(fd)
            f.close()
            del watches[fd]
            print(f"Unwatched {service}")
            return


def watch(services, webhook_url, auto_discover):
    """Watch memory.events for the given services using poll()."""
    poller = select.poll()
    watches = {}  # fd -> (service, file, cgroup_path, prev_counters)
    watched_services = set()

    for service in services:
        add_watch(poller, watches, service)
        watched_services.add(service)

    print(f"Monitoring {len(watches)} service(s)...")

    timeout = REDISCOVER_MS if auto_discover else None

    while True:
        events = poller.poll(timeout)

        for fd, _event in events:
            service, f, cgroup_path, prev = watches[fd]
            events_path = f"{cgroup_path}/memory.events"
            current = read_events(events_path)

            # Find counters that increased
            changes = {}
            for key in COUNTERS:
                old = prev.get(key, 0)
                new = current.get(key, 0)
                if new > old:
                    changes[key] = (old, new)

            if changes:
                current_mb = read_current_mb(cgroup_path)
                alert(service, current_mb, changes, webhook_url)

            # Update saved state
            watches[fd] = (service, f, cgroup_path, current)

            # Re-read to re-arm poll
            f.seek(0)
            f.read()

        # Re-discover services
        if auto_discover:
            current_services = set(discover_services())

            # Watch new services
            for service in current_services - watched_services:
                try:
                    add_watch(poller, watches, service)
                    watched_services.add(service)
                except Exception as e:
                    print(f"Warning: failed to watch {service}: {e}", file=sys.stderr)

            # Remove services that no longer have limits
            for service in watched_services - current_services:
                remove_watch(poller, watches, service)
                watched_services.discard(service)


def main():
    parser = argparse.ArgumentParser(description="Watch systemd service memory limits")
    parser.add_argument("services", nargs="*", help="systemd service names to watch (auto-discovers if omitted)")
    parser.add_argument("--webhook", default=os.environ.get("MEMORY_ALERT_WEBHOOK", ""),
                        help="Webhook URL for alerts")
    args = parser.parse_args()

    services = args.services
    auto_discover = not services
    if auto_discover:
        services = discover_services()
        if not services:
            print("No services with memory limits found, waiting for new services...")
        else:
            print(f"Auto-discovered: {', '.join(services)}")

    watch(services, args.webhook, auto_discover)


if __name__ == "__main__":
    main()
