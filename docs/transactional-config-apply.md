# Transactional Viewpie configuration apply

This path is for applying a **complete candidate YAML** from Home Assistant. It is separate from the regular Viewpie API and never grants the HA integration arbitrary shell execution.

## Transaction helper

`scripts/viewpie-apply-config.sh` validates the candidate with `viewpie --validate-config`, locks concurrent applies, creates a timestamped backup, stops Viewpie, atomically replaces the YAML, starts the service and waits for `/api/status`. On failure it restores the backup and checks the old service. Results are `OK` (exit 0), `ROLLED_BACK` (exit 2) or `CRITICAL` (exit 1 with critical output).

## Authenticated bridge

`scripts/viewpie-config-bridge.py` is a small standard-library HTTP server. It accepts only `POST /apply`, requires a bearer token, caps request bodies at 1 MiB and invokes the helper with fixed server-side paths. It does not accept paths, service names or shell commands from HTTP callers.

Install both scripts on the Pi:

```bash
sudo install -o root -g root -m 0755 scripts/viewpie-apply-config.sh /usr/local/sbin/viewpie-apply-config
sudo install -o root -g root -m 0755 scripts/viewpie-config-bridge.py /usr/local/libexec/viewpie-config-bridge.py
```

Create a dedicated unprivileged service account (example `viewpiebridge`) and a sudoers rule that permits only the fixed helper. Since sudoers matches arguments literally, a small root-owned wrapper is preferable: configure it to hard-code the active config/service/binary/health URL, accept exactly one candidate path under a dedicated staging directory, and call the transactional helper. Do not grant `NOPASSWD: ALL` or allow the bridge account to modify the helper/config/service files.

Configure the bridge with environment variables:

- `VIEWPIE_BRIDGE_TOKEN`: long random secret (at least 32 random bytes, hex-encoded).
- `VIEWPIE_BRIDGE_HOST`: defaults to `127.0.0.1` (recommended).
- `VIEWPIE_BRIDGE_PORT`: defaults to `8765`.
- `VIEWPIE_APPLY_HELPER`, `VIEWPIE_CONFIG`, `VIEWPIE_SERVICE`, `VIEWPIE_BINARY`, `VIEWPIE_HEALTH_URL`: fixed target paths/health endpoint.

If Home Assistant runs on a different host, keep the bridge on loopback and expose it through a TLS reverse proxy with authentication/IP allow-listing. Do not bind this bearer-token HTTP service to an untrusted network without TLS. Restrict access to the token and never put it in public dashboard YAML.

## Home Assistant service

Use `viewpie.apply_config` from Developer Tools → Actions. Provide the complete YAML text and the bridge URL/token. The integration sends the YAML to `POST <bridge_url>/apply` with `Content-Type: application/yaml`. It reports `OK`, `ROLLED_BACK` or a critical/error result. Use HTTPS except for loopback URLs. This is a destructive operation: back up the current YAML and test a known-good candidate first.

## Limits

The health probe only confirms that the API answers; it does not prove all RTSP streams decode or the display looks correct. A `CRITICAL` result needs manual inspection of the Pi and backup files. Install and test the bridge on the target Raspberry Pi before relying on it for production changes.
