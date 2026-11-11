#!/usr/bin/python3
"""Privileged, narrowly-scoped Viewpie self-updater.

Started only through a systemd-activated Unix socket owned by group viewpie.
The client can request one fixed operation; release metadata, architecture,
asset selection and SHA-256 are independently verified here as root.
"""
import fcntl
import hashlib
import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

RELEASE_API = "https://api.github.com/repos/Shnyak/viewpie/releases/latest"
CACHE_DIR = Path("/var/cache/viewpie-update")
LOCK_PATH = Path("/run/lock/viewpie-update.lock")
USER_AGENT = "Viewpie-updater"


def log(message):
    print(message, file=sys.stderr, flush=True)


def request_json(url):
    req = urllib.request.Request(
        url, headers={"Accept": "application/vnd.github+json", "User-Agent": USER_AGENT}
    )
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.load(response)


def main():
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM, fileno=os.dup(3))
    connection, _ = server.accept()
    with connection:
        command = connection.makefile("rb").readline(64).decode("ascii", "replace").strip()
        if command != "INSTALL":
            connection.sendall(b'{"ok":false,"error":"unsupported command"}\\n')
            return 2
        connection.sendall(b'{"ok":true,"status":"started"}\\n')

    LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    with LOCK_PATH.open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            log("Viewpie update already running")
            return 1

        release = request_json(RELEASE_API)
        tag = str(release.get("tag_name", ""))
        version = tag[1:] if tag.startswith("v") else tag
        if not re.fullmatch(r"\\d+\\.\\d+\\.\\d+(?:-[A-Za-z0-9.]+)?", version):
            raise RuntimeError("GitHub returned an invalid release tag")

        arch = subprocess.check_output(
            ["dpkg", "--print-architecture"], text=True, timeout=10
        ).strip()
        if arch not in ("armhf", "arm64"):
            raise RuntimeError("Unsupported Debian architecture: " + arch)

        prefix = "viewpie_" + version + "-"
        suffix = "_" + arch + ".deb"
        candidates = [
            asset for asset in release.get("assets", [])
            if str(asset.get("name", "")).startswith(prefix)
            and str(asset.get("name", "")).endswith(suffix)
        ]
        if len(candidates) != 1:
            raise RuntimeError("The latest release has no unique package for " + arch)

        asset = candidates[0]
        digest = str(asset.get("digest") or "")
        if not digest.startswith("sha256:") or not re.fullmatch(r"sha256:[0-9a-fA-F]{64}", digest):
            raise RuntimeError("GitHub release asset has no verifiable SHA-256 digest")

        CACHE_DIR.mkdir(mode=0o700, parents=True, exist_ok=True)
        fd, temp_name = tempfile.mkstemp(prefix="viewpie-", suffix=".deb", dir=CACHE_DIR)
        os.fchmod(fd, 0o600)
        os.close(fd)
        package = Path(temp_name)
        try:
            req = urllib.request.Request(
                asset["browser_download_url"], headers={"User-Agent": USER_AGENT}
            )
            hasher = hashlib.sha256()
            with urllib.request.urlopen(req, timeout=60) as response, package.open("wb") as out:
                while True:
                    chunk = response.read(1024 * 1024)
                    if not chunk:
                        break
                    hasher.update(chunk)
                    out.write(chunk)
            if hasher.hexdigest().lower() != digest.split(":", 1)[1].lower():
                raise RuntimeError("Downloaded package SHA-256 does not match GitHub release metadata")

            log("Installing Viewpie " + version + " (" + arch + ")")
            subprocess.run(["dpkg", "-i", str(package)], check=True, timeout=900)
            log("Viewpie update completed: " + version)
        finally:
            try:
                package.unlink()
            except FileNotFoundError:
                pass
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        log("Viewpie update failed: " + str(exc))
        raise SystemExit(1)
