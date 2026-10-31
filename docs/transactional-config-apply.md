# Transactional Viewpie configuration apply

This flow applies a **complete candidate YAML** from Home Assistant. It uses a restricted bridge and a fixed-target root wrapper; it does not expose arbitrary shell execution through the Viewpie camera API.

## Transaction helper

`scripts/viewpie-apply-config.sh` validates the candidate with `viewpie --validate-config`, serializes requests with `flock`, creates a timestamped backup, stops Viewpie, atomically replaces the YAML, starts the service and waits for `/api/status`. If the new service does not become healthy, it restores the backup and probes the previous service. Results are `OK` (exit 0), `ROLLED_BACK` (exit 2) or `CRITICAL` (exit 1 with critical output).

## Install the helper and fixed-target wrapper on the Pi

Review and adjust the fixed config/service/binary/health URL in `scripts/viewpie-apply-config-wrapper` first if your installation differs from the defaults.

```bash
sudo install -o root -g root -m 0755 scripts/viewpie-apply-config.sh /usr/local/sbin/viewpie-apply-config
sudo install -o root -g root -m 0755 scripts/viewpie-apply-config-wrapper /usr/local/sbin/viewpie-apply-config-wrapper
sudo install -o root -g root -m 0755 scripts/viewpie-config-bridge.py /usr/local/libexec/viewpie-config-bridge.py
sudo useradd --system --home /var/lib/viewpie-config-bridge --shell /usr/sbin/nologin viewpiebridge
sudo install -d -o viewpiebridge -g viewpiebridge -m 0700 /var/lib/viewpie-config-bridge/incoming
```

The bridge user must be allowed to run **only** the root-owned wrapper without a password. Create `/etc/sudoers.d/viewpie-config-bridge` with:

```sudoers
viewpiebridge ALL=(root) NOPASSWD: /usr/local/sbin/viewpie-apply-config-wrapper *
```

Then validate it with `sudo visudo -cf /etc/sudoers.d/viewpie-config-bridge`. The wrapper rejects all arguments except a candidate path inside the staging directory and hard-codes the target configuration path, systemd service, binary and health URL. Keep the wrapper and helper root-owned and not writable by `viewpiebridge`.

## Bridge service

Generate a token with `openssl rand -hex 32` and keep it private. Create a systemd unit such as `/etc/systemd/system/viewpie-config-bridge.service`:

```ini
[Unit]
Description=Authenticated Viewpie transactional config bridge
After=network.target viewpie.service
Wants=viewpie.service

[Service]
Type=simple
User=viewpiebridge
Group=viewpiebridge
EnvironmentFile=/etc/viewpie-config-bridge.env
ExecStart=/usr/bin/python3 /usr/local/libexec/viewpie-config-bridge.py
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/viewpie-config-bridge/incoming
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
LockPersonality=true
RestrictSUIDSGID=true

[Install]
WantedBy=multi-user.target
```

Create root-owned `/etc/viewpie-config-bridge.env` (mode `0600`):

```bash
VIEWPIE_BRIDGE_HOST=127.0.0.1
VIEWPIE_BRIDGE_PORT=8765
VIEWPIE_BRIDGE_TOKEN=PASTE_THE_RANDOM_TOKEN_HERE
```

Start it with `sudo systemctl daemon-reload && sudo systemctl enable --now viewpie-config-bridge`.

The bridge defaults to loopback. If Home Assistant is on another host, expose it only through a TLS reverse proxy with an IP allow-list and authentication; do not bind this bearer-token HTTP service directly to an untrusted network. Use the same token in the HA service call. Do not store the token in public dashboard YAML.

## Home Assistant

After updating the custom integration, restart Home Assistant. Use **Developer Tools → Actions → `viewpie.apply_config`** with the full candidate YAML, e.g.:

```yaml
action: viewpie.apply_config
data:
  bridge_url: https://YOUR-PI-HOST/config-bridge
  bridge_token: YOUR_RANDOM_TOKEN
  config_yaml: |-
    display:
      spacing: 2
    feeds:
      # Include the complete feeds section from your current config.
      yard:
        url: rtsp://user:password@camera/stream
    panels:
      # Include every panel and all other required sections.
      - id: outside
        name: Outside
        layout: |-
          a b
        viewports:
          - slot: a
            feeds: [yard]
```

The YAML above is only a shape example, **not a ready-to-apply config**. Use the complete schema accepted by your installed Viewpie version; omitting existing feeds, panels or required fields can change the whole configuration. The integration requires HTTPS for non-loopback bridge URLs and reports whether the apply succeeded, rolled back or needs manual recovery.

## Safety and limits

- Back up the current YAML before the first test. Start with a known-good copy before making layout changes.
- The helper retains timestamped backups for manual recovery.
- The health check proves only that `/api/status` answers. It cannot prove every RTSP feed decodes or the display looks correct.
- If a `CRITICAL` result occurs, inspect the Pi and backups manually; do not repeat applies blindly.
- Do not expose the Viewpie API or bridge to the public internet.
