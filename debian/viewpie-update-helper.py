#!/usr/bin/python3
"""Privileged, narrowly-scoped Viewpie self-updater.

The socket-activated front end accepts one fixed operation from the viewpie
service account, then starts a separate transient systemd worker. The worker
survives package hooks restarting the API and updater socket units.
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
import uuid
from pathlib import Path

RELEASE_API = "https://api.github.com/repos/Shnyak/viewpie/releases/latest"
CACHE_DIR = Path("/var/cache/viewpie-update")
STATE_DIR = Path("/var/lib/viewpie")
STATUS_PATH = STATE_DIR / "update-status.json"
LOG_PATH = STATE_DIR / "update.log"
LOCK_PATH = Path("/run/lock/viewpie-update.lock")
USER_AGENT = "Viewpie-updater"
CURRENT = {"status": "idle", "phase": "idle", "message": "Обновление ещё не запускалось"}


def _write_status(**changes):
    CURRENT.update(changes)
    CURRENT["updated_at"] = __import__("datetime").datetime.now(
        __import__("datetime").timezone.utc
    ).isoformat()
    STATE_DIR.mkdir(mode=0o755, parents=True, exist_ok=True)
    temp = STATUS_PATH.with_suffix(".tmp")
    temp.write_text(json.dumps(CURRENT, ensure_ascii=False) + "\n", encoding="utf-8")
    os.chmod(temp, 0o644)
    temp.replace(STATUS_PATH)


def log(message):
    safe = str(message).replace("\x00", "")
    timestamp = __import__("datetime").datetime.now(
        __import__("datetime").timezone.utc
    ).isoformat()
    entry = f"{timestamp} {safe}"
    print(entry, file=sys.stderr, flush=True)
    STATE_DIR.mkdir(mode=0o755, parents=True, exist_ok=True)
    with LOG_PATH.open("a", encoding="utf-8") as stream:
        stream.write(entry + "\n")
    os.chmod(LOG_PATH, 0o644)

def event(phase, message, status="running", **extra):
    _write_status(status=status, phase=phase, message=message, **extra)
    log(f"[{phase}] {message}")


def run_logged(command, *, timeout_seconds, phase):
    printable = " ".join(str(part) for part in command)
    event(phase, f"$ {printable}")
    process = subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, bufsize=1,
    )
    try:
        assert process.stdout is not None
        for line in process.stdout:
            log(line.rstrip())
        code = process.wait(timeout=timeout_seconds)
    except Exception:
        process.kill()
        process.wait()
        raise
    log(f"[{phase}] exit code: {code}")
    if code:
        raise subprocess.CalledProcessError(code, command)
    return code


def request_json(url):
    req = urllib.request.Request(
        url, headers={"Accept": "application/vnd.github+json", "User-Agent": USER_AGENT}
    )
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.load(response)


def read_deb_field(package, field):
    """Read one control field without relying on multi-field dpkg-deb formatting."""
    event("metadata", f"$ dpkg-deb -f {package.name} {field}")
    output = subprocess.check_output(
        ["dpkg-deb", "-f", str(package), field],
        text=True,
        timeout=10,
    ).strip()
    # dpkg-deb versions/platforms may print either the bare value or
    # "Field: value". Accept both, but never silently accept multiple lines.
    lines = output.splitlines()
    if len(lines) != 1:
        raise RuntimeError("Downloaded package metadata is incomplete")
    value = lines[0].strip()
    prefix = field + ":"
    if value.startswith(prefix):
        value = value[len(prefix):].strip()
    if not value:
        raise RuntimeError("Downloaded package metadata is incomplete")
    return value


def install_latest_release():
    _write_status(
        status="running", phase="starting", message="Запуск установщика",
        started_at=__import__("datetime").datetime.now(
            __import__("datetime").timezone.utc
        ).isoformat(),
        finished_at=None, exit_code=None,
    )
    LOG_PATH.write_text("", encoding="utf-8")
    os.chmod(LOG_PATH, 0o644)
    LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    with LOCK_PATH.open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            event("lock", "Другое обновление уже выполняется", "failed", exit_code=1)
            return 1

        event("release", "Получение метаданных последнего релиза")
        release = request_json(RELEASE_API)
        tag = str(release.get("tag_name", ""))
        version = tag[1:] if tag.startswith("v") else tag
        if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?", version):
            raise RuntimeError("GitHub returned an invalid release tag")

        event("prepare", "$ dpkg --print-architecture")
        arch = subprocess.check_output(
            ["dpkg", "--print-architecture"], text=True, timeout=10
        ).strip()
        _write_status(version=version, architecture=arch)
        event("prepare", f"Выбран релиз {version}, архитектура {arch}")
        event("prepare", "$ dpkg-query -W -f=${Version} viewpie")
        installed_package_version = subprocess.check_output(
            ["dpkg-query", "-W", "-f=${Version}", "viewpie"],
            text=True,
            timeout=10,
        ).strip()
        installed_release_version = installed_package_version.rsplit("-", 1)[0]
        if installed_release_version == version:
            event("complete", "Уже установлена последняя версия " + version, "success",
                  exit_code=0, finished_at=__import__("datetime").datetime.now(
                      __import__("datetime").timezone.utc
                  ).isoformat())
            return 0
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

        event("download", "Загрузка Debian-пакета " + str(asset.get("name", "")))
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
                total = int(response.headers.get("Content-Length", "0") or 0)
                downloaded = 0
                last_reported = -10
                while True:
                    chunk = response.read(1024 * 1024)
                    if not chunk:
                        break
                    hasher.update(chunk)
                    out.write(chunk)
                    downloaded += len(chunk)
                    if total:
                        percent = min(100, downloaded * 100 // total)
                        if percent >= last_reported + 10 or percent == 100:
                            _write_status(progress_percent=percent, downloaded_bytes=downloaded, total_bytes=total)
                            log(f"[download] {percent}% ({downloaded}/{total} bytes)")
                            last_reported = percent // 10 * 10
                    elif downloaded // (5 * 1024 * 1024) > (downloaded - len(chunk)) // (5 * 1024 * 1024):
                        log(f"[download] downloaded {downloaded} bytes")
            event("verify", "$ SHA-256 проверка загруженного пакета")
            if hasher.hexdigest().lower() != digest.split(":", 1)[1].lower():
                raise RuntimeError("Downloaded package SHA-256 does not match GitHub release metadata")

            package_name = read_deb_field(package, "Package")
            package_version = read_deb_field(package, "Version")
            package_arch = read_deb_field(package, "Architecture")
            if (
                package_name != "viewpie"
                or package_version.rsplit("-", 1)[0] != version
                or package_arch != arch
            ):
                raise RuntimeError("Downloaded package identity does not match the selected release")

            event("verify", "SHA-256 и метаданные пакета подтверждены")
            _write_status(package_version=package_version)
            _write_status(progress_percent=None)
            event("install", "$ dpkg -i " + package.name + " — установка " + package_version)
            run_logged(["dpkg", "-i", str(package)], timeout_seconds=900, phase="dpkg")
            event("complete", "Обновление Viewpie завершено: " + version, "success",
                  exit_code=0, finished_at=__import__("datetime").datetime.now(
                      __import__("datetime").timezone.utc
                  ).isoformat())
        finally:
            try:
                package.unlink()
            except FileNotFoundError:
                pass
    return 0


def main():
    if sys.argv[1:] == ["--apply"]:
        return install_latest_release()

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM, fileno=os.dup(3))
    connection, _ = server.accept()
    with connection:
        command = connection.makefile("rb").readline(64).decode("ascii", "replace").strip()
        if command != "INSTALL":
            connection.sendall(b"0")
            return 2

        # Run the actual dpkg transaction in a transient systemd unit. Debian's
        # package hooks may restart Viewpie and the socket listener, but must
        # not kill the worker that is performing the upgrade.
        unit = "viewpie-update-worker-" + uuid.uuid4().hex
        event("queue", "$ systemd-run --unit=" + unit + " /usr/bin/python3 update-helper.py --apply")
        try:
            subprocess.run(
                [
                    "systemd-run",
                    "--quiet",
                    "--collect",
                    "--unit=" + unit,
                    "/usr/bin/python3",
                    __file__,
                    "--apply",
                ],
                check=True,
                timeout=15,
            )
        except Exception:
            connection.sendall(b"0")
            raise
        connection.sendall(b"1")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        message = "Viewpie update failed: " + str(exc)
        try:
            event("failed", message, "failed", error=str(exc), exit_code=1,
                  finished_at=__import__("datetime").datetime.now(
                      __import__("datetime").timezone.utc
                  ).isoformat())
        except Exception:
            log(message)
        raise SystemExit(1)
