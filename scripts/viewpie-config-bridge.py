#!/usr/bin/env python3
"""Small authenticated HTTP bridge for transactional Viewpie YAML apply.

Run unprivileged. Grant only this script's service account passwordless sudo
for the fixed helper invocation documented in docs/transactional-config-apply.md.
Prefer loopback + an authenticated TLS reverse proxy when HA is on another host.
"""
from __future__ import annotations

import hmac
import json
import os
import subprocess
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HOST = os.environ.get("VIEWPIE_BRIDGE_HOST", "127.0.0.1")
PORT = int(os.environ.get("VIEWPIE_BRIDGE_PORT", "8765"))
TOKEN = os.environ.get("VIEWPIE_BRIDGE_TOKEN", "")
WRAPPER = os.environ.get("VIEWPIE_APPLY_WRAPPER", "/usr/local/sbin/viewpie-apply-config-wrapper")
STAGING = os.environ.get("VIEWPIE_BRIDGE_STAGING", "/var/lib/viewpie-config-bridge/incoming")
CONFIG = os.environ.get("VIEWPIE_CONFIG", "/etc/viewpie/viewpie.yaml")
SERVICE = os.environ.get("VIEWPIE_SERVICE", "viewpie.service")
BINARY = os.environ.get("VIEWPIE_BINARY", "/usr/bin/viewpie")
HEALTH_URL = os.environ.get("VIEWPIE_HEALTH_URL", "http://127.0.0.1:8080/api/status")
MAX_BODY = 1024 * 1024


class Handler(BaseHTTPRequestHandler):
    server_version = "ViewpieConfigBridge/1.0"

    def _json(self, status: int, payload: dict) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        if self.path != "/apply":
            self._json(404, {"status": "ERROR", "message": "not found"})
            return
        if not TOKEN:
            self._json(503, {"status": "ERROR", "message": "bridge token is not configured"})
            return
        supplied = self.headers.get("Authorization", "")
        if not hmac.compare_digest(supplied, "Bearer " + TOKEN):
            self._json(401, {"status": "ERROR", "message": "unauthorized"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        if length < 1 or length > MAX_BODY:
            self._json(413, {"status": "ERROR", "message": "body must be 1..1048576 bytes"})
            return
        if "yaml" not in self.headers.get("Content-Type", "").lower() and "text/plain" not in self.headers.get("Content-Type", "").lower():
            self._json(415, {"status": "ERROR", "message": "use application/yaml"})
            return
        raw = self.rfile.read(length)
        try:
            candidate_text = raw.decode("utf-8")
        except UnicodeDecodeError:
            self._json(400, {"status": "ERROR", "message": "candidate must be UTF-8"})
            return
        if not candidate_text.strip():
            self._json(400, {"status": "ERROR", "message": "candidate is empty"})
            return

        # Helper itself copies the candidate to the config filesystem, validates,
        # backs up, applies, probes health and rolls back on failure.
        Path(STAGING).mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", suffix=".yaml", dir=STAGING, delete=False) as candidate:
            candidate.write(candidate_text)
            candidate_path = candidate.name
        try:
            proc = subprocess.run(
                ["sudo", "-n", WRAPPER, candidate_path],
                capture_output=True, text=True, timeout=150, check=False,
                env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LANG": "C.UTF-8"},
            )
            output = (proc.stdout + "\n" + proc.stderr).strip()[-6000:]
            if proc.returncode == 0 and "OK:" in output:
                status, http_status = "OK", 200
            elif proc.returncode == 2 and "ROLLED_BACK:" in output:
                status, http_status = "ROLLED_BACK", 409
            else:
                status, http_status = "CRITICAL" if "CRITICAL:" in output else "ERROR", 500
            self._json(http_status, {"status": status, "returncode": proc.returncode, "output": output})
        except subprocess.TimeoutExpired:
            self._json(504, {"status": "CRITICAL", "message": "apply helper timed out; inspect Pi service and configuration"})
        except OSError as exc:
            self._json(500, {"status": "ERROR", "message": str(exc)})
        finally:
            Path(candidate_path).unlink(missing_ok=True)

    def log_message(self, fmt: str, *args) -> None:
        # Do not log headers, bearer tokens or candidate YAML (which may contain credentials).
        print("viewpie-config-bridge:", fmt % args, flush=True)


if not TOKEN:
    raise SystemExit("Set VIEWPIE_BRIDGE_TOKEN to a long random secret before starting")
ThreadingHTTPServer((HOST, PORT), Handler).serve_forever()
