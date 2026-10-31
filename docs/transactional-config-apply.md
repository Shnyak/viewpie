# Transactional Viewpie configuration apply

The helper `scripts/viewpie-apply-config.sh` is the system-level part of the
configuration transaction. It must run as root (it calls `systemctl`), and
must only be callable by a trusted local component. Do **not** expose it directly
as a network command or grant broad passwordless sudo access.

## What it guarantees

1. Validates a candidate with `viewpie --validate-config` before stopping the service.
2. Serializes apply requests with `flock`.
3. Copies the current config to a timestamped backup.
4. Stops Viewpie, atomically replaces the YAML file, and starts the service.
5. Waits up to 30 seconds for `GET /api/status` to answer.
6. If startup/health fails, restores the backup and attempts to restart the old config.
7. Prints a distinct result: `OK`, `ROLLED_BACK`, or `CRITICAL`.

Example (candidate must already contain the complete new YAML):

```bash
sudo scripts/viewpie-apply-config.sh \
  /tmp/viewpie-candidate.yaml \
  /etc/viewpie/viewpie.yaml \
  viewpie.service \
  /usr/bin/viewpie \
  http://127.0.0.1:8080/api/status
```

## Important limits

- The helper does not itself provide an HTTP endpoint or Home Assistant UI. The
  trusted HA-to-Pi command bridge still needs to submit a candidate and return
  this helper's result to HA.
- The health probe verifies that the API is responding; it cannot prove that
  every RTSP camera produces decodable frames or that the physical display is
  rendering correctly.
- Backups are intentionally retained for manual recovery. Set a retention policy
  only after deciding how many known-good versions to keep.
- Install the helper root-owned and non-writable by the Viewpie service user.
  Configure the actual service name, binary path, config path and health URL
  for the target Pi before using it.
