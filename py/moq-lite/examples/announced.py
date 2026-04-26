"""List broadcasts announced on a relay under a given prefix.

python py/moq-lite/examples/announced.py --url https://relay.example.com
python py/moq-lite/examples/announced.py --url https://relay.example.com --prefix live/
"""

import argparse
import asyncio

import moq_lite as moq


async def run(url: str, prefix: str, tls_verify: bool) -> None:
    async with moq.Client(url, tls_verify=tls_verify) as client:
        print(f"watching announcements under {prefix!r} at {url}")
        async for announcement in client.announced(prefix):
            print(f"  + {announcement.path}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="relay URL (https://...)")
    parser.add_argument("--prefix", default="", help="only show broadcasts under this prefix")
    parser.add_argument("--no-tls-verify", action="store_true", help="disable TLS verification (dev only)")
    args = parser.parse_args()

    try:
        asyncio.run(run(args.url, args.prefix, tls_verify=not args.no_tls_verify))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
