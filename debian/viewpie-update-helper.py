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
import selectors
import signal
import socket
import subprocess
import sys
import tempfile
import time
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
    environment = os.environ.copy()
    environment.update({
        "DEBIAN_FRONTEND": "noninteractive",
        "DEBIAN_PRIORITY": "critical",
        "APT_LISTCHANGES_FRONTEND": "none",
    })
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=environment,
        bufsize=0,
        start_new_session=True,
    )
    assert process.stdout is not None
    descriptor = process.stdout.fileno()
    os.set_blocking(descriptor, False)
    selector = selectors.DefaultSelector()
    selector.register(descriptor, selectors.EVENT_READ)
    pending = bytearray()
    deadline = time.monotonic() + timeout_seconds
    try:
        # Drain output incrementally, but keep the deadline active while either
        # the process runs or a child keeps the pipe open after its parent exits.
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"{phase} exceeded its {timeout_seconds}s deadline")
            for _key, _mask in selector.select(min(remaining, 1.0)):
                chunk = os.read(descriptor, 65536)
                if not chunk:
                    selector.unregister(descriptor)
                    break
                pending.extend(chunk)
                while True:
                    newline = pending.find(b"\n")
                    if newline < 0:
                        break
                    line = bytes(pending[:newline]).decode("utf-8", errors="replace").rstrip("\r")
                    del pending[:newline + 1]
                    log(line)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"{phase} exceeded its {timeout_seconds}s deadline")
        code = process.wait(timeout=remaining)
    except BaseException:
        # apt-get may have spawned dpkg/debconf children. Terminate the whole
        # process group so a timed-out update cannot leave package locks held.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        raise
    finally:
        selector.close()
        process.stdout.close()
    if pending:
        log(bytes(pending).decode("utf-8", errors="replace").rstrip("\\r"))
    log(f"[{phase}] exit code: {code}")
    if code:
        raise subprocess.CalledProcessError(code, command)
    return code


def package_is_installed(name):
    """Return true only when dpkg reports the package fully installed."""
    try:
        status = subprocess.check_output(
            ["dpkg-query", "-W", "-f=${db:Status-Status}", name],
            text=True,
            timeout=10,
        ).strip()
    except subprocess.CalledProcessError:
        return False
    return status == "installed"


def missing_runtime_packages():
    """Only request runtime packages that are genuinely absent."""
    return [name for name in ("fonts-dejavu-core", "ffmpeg") if not package_is_installed(name)]


def installed_package_version(name):
    if not package_is_installed(name):
        return "not installed"
    return subprocess.check_output(
        ["dpkg-query", "-W", "-f=${Version}", name],
        text=True,
        timeout=10,
    ).strip()


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


def install_latest_release(preserve_log=False):
    LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    reboot_required = False
    with LOCK_PATH.open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            log("Another Viewpie update is already running; duplicate worker skipped")
            return 0

        _write_status(
            status="running", phase="starting", message="Запуск установщика",
            worker_unit=os.environ.get("VIEWPIE_UPDATE_UNIT"),
            started_at=__import__("datetime").datetime.now(
                __import__("datetime").timezone.utc
            ).isoformat(),
            finished_at=None, exit_code=None,
        )
        if not preserve_log:
            LOG_PATH.write_text("", encoding="utf-8")
            os.chmod(LOG_PATH, 0o644)
        event("release", "$ GET " + RELEASE_API)
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

        event("download", "$ GET " + str(asset["browser_download_url"]) + " -> " + str(asset.get("name", "")))
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
            # Install runtime prerequisites only when absent. In particular,
            # never upgrade an already-installed FFmpeg as a side effect of a
            # Viewpie update: decoder behavior depends on the distro build.
            missing = missing_runtime_packages()
            event("dependencies", "Refreshing apt metadata before resolving package dependencies")
            run_logged(["apt-get", "update"], timeout_seconds=900, phase="apt-update")
            if missing:
                event("dependencies", "Missing runtime packages: " + ", ".join(missing))
                run_logged(
                    ["apt-get", "install", "-y", "--no-install-recommends", "--no-upgrade", *missing],
                    timeout_seconds=900,
                    phase="dependencies",
                )
            else:
                event("dependencies", "FFmpeg and diagnostics fonts are already installed; leaving their versions unchanged")
            ffmpeg_version = installed_package_version("ffmpeg")
            _write_status(ffmpeg_version=ffmpeg_version)
            event("dependencies", "Using FFmpeg " + ffmpeg_version + "; no FFmpeg upgrade requested")

            # Let apt resolve the local package's dependencies and configure
            # the transaction as a whole. --no-upgrade prevents this Viewpie
            # update from silently replacing existing runtime libraries.
            event("install", "$ apt-get install local package " + package.name + " — " + package_version)
            run_logged(
                [
                    "apt-get", "install", "-y", "--no-install-recommends", "--no-upgrade",
                    "-o", "Dpkg::Options::=--force-confdef",
                    "-o", "Dpkg::Options::=--force-confold",
                    str(package),
                ],
                timeout_seconds=900,
                phase="install",
            )
            event("complete", "Обновление Viewpie завершено: " + version, "success",
                  reboot_pending=True, exit_code=0,
                  finished_at=__import__("datetime").datetime.now(
                      __import__("datetime").timezone.utc
                  ).isoformat())
            reboot_required = True
        finally:
            try:
                package.unlink()
            except FileNotFoundError:
                pass
    if reboot_required:
        # Give Home Assistant a short window to read the successful update
        # journal before the host goes down. Reboot only after dpkg completed.
        import time
        time.sleep(3)
        log("[reboot] Requesting a full Raspberry Pi reboot after successful installation")
        subprocess.run(["systemctl", "reboot"], check=True, timeout=10)
    return 0


def main():
    if sys.argv[1:] == ["--apply", "--preserve-log"]:
        return install_latest_release(preserve_log=True)
    if sys.argv[1:] == ["--apply"]:
        return install_latest_release()

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM, fileno=os.dup(3))
    connection, _ = server.accept()
    with connection:
        command = connection.makefile("rb").readline(64).decode("ascii", "replace").strip()
        if command != "INSTALL":
            connection.sendall(b"0")
            return 2

        try:
            existing = json.loads(STATUS_PATH.read_text(encoding="utf-8"))
        except (FileNotFoundError, json.JSONDecodeError, OSError):
            existing = {}
        if existing.get("status") == "running":
            active_unit = str(existing.get("worker_unit") or "")
            unit_state = ""
            if active_unit:
                try:
                    check = subprocess.run(
                        ["systemctl", "is-active", active_unit],
                        capture_output=True, text=True, timeout=5,
                    )
                    unit_state = check.stdout.strip()
                except Exception as error:
                    log("Could not inspect previous updater worker: " + str(error))
            if unit_state in ("active", "activating", "reloading"):
                log("Update already running; reusing worker " + active_unit)
                connection.sendall(b"1")
                return 0
            log("Previous updater state is stale; its systemd worker is not active")

        # Run the actual dpkg transaction in a transient systemd unit. Debian's
        # package hooks may restart Viewpie and the socket listener, but must
        # not kill the worker that is performing the upgrade.
        unit = "viewpie-update-worker-" + uuid.uuid4().hex
        STATE_DIR.mkdir(mode=0o755, parents=True, exist_ok=True)
        LOG_PATH.write_text("", encoding="utf-8")
        os.chmod(LOG_PATH, 0o644)
        event("queue", "$ systemd-run --unit=" + unit + " /usr/bin/python3 update-helper.py --apply --preserve-log", worker_unit=unit)
        try:
            subprocess.run(
                [
                    "systemd-run",
                    "--quiet",
                    "--collect",
                    "--unit=" + unit,
                    "--setenv=VIEWPIE_UPDATE_UNIT=" + unit,
                    "/usr/bin/python3",
                    __file__,
                    "--apply",
                    "--preserve-log",
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
