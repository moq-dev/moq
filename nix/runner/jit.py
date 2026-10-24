"""Mint one ephemeral runner configuration without exporting the App credential."""

import base64
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

import jwt


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RuntimeError("GitHub API redirects are refused")


def request(path, token, body):
    req = urllib.request.Request(
        "https://api.github.com" + path,
        data=json.dumps(body).encode(),
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": "Bearer " + token,
            "X-GitHub-Api-Version": "2026-03-10",
            "User-Agent": "github-runner-vm",
        },
        method="POST",
    )
    try:
        with urllib.request.build_opener(NoRedirect).open(req, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        # GitHub error bodies can contain request details. Never log credentials.
        raise RuntimeError(f"GitHub {path} returned HTTP {error.code}") from None


def mint(config, key, post=request, now=None):
    now = int(time.time()) if now is None else now
    assertion = jwt.encode(
        {"iat": now - 60, "exp": now + 540, "iss": str(config["appId"])},
        key,
        algorithm="RS256",
    )
    owner, repo = config["repository"].split("/")
    installation = post(
        f"/app/installations/{config['installationId']}/access_tokens",
        assertion,
        {"repositories": [repo], "permissions": {"administration": "write"}},
    )
    token = installation["token"]
    response = post(
        f"/repos/{owner}/{repo}/actions/runners/generate-jitconfig",
        token,
        {
            # A failed boot may leave an unused registration behind. Never replace it.
            "name": config["name"] + "-" + uuid.uuid4().hex,
            "runner_group_id": config["groupId"],
            "labels": ["self-hosted", "Linux", "X64", *config["labels"]],
            "work_folder": "/var/cache/github-runner-work",
        },
    )
    encoded = response["encoded_jit_config"]
    settings = json.loads(json.loads(base64.b64decode(encoded, validate=True))[".runner"])
    if settings.get("ephemeral") is not True:
        raise RuntimeError("GitHub returned a non-ephemeral runner configuration")
    return encoded


def main():
    config = json.loads(Path(sys.argv[1]).read_text())
    key = (Path(os.environ["CREDENTIALS_DIRECTORY"]) / "app-key").read_text()
    encoded = mint(config, key)
    destination = Path(sys.argv[2])
    # Replace atomically; neither a stale config nor a symlink gets followed.
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            os.fchmod(output.fileno(), 0o440)
            output.write(encoded.encode())
            output.flush()
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        sys.exit(str(error))
    except Exception as error:
        sys.exit(f"Cannot mint runner JIT configuration: {type(error).__name__}")
