# Protected Raspberry Pi restart

The `POST /api/restart` endpoint is disabled unless `VIEWPIE_RESTART_TOKEN`
is set in the Viewpie service environment. It accepts only
`Authorization: Bearer <token>` and invokes the fixed command
`sudo -n /usr/sbin/reboot`.

For the Home Assistant integration, enter the same random token in its
existing **API key** field. The integration sends it as a bearer token for
API requests; other Viewpie endpoints do not gain additional permissions
from this setting. Do not reuse a password or publish this token.

On Raspberry Pi OS, create a random token and an environment file readable
only by root:

```bash
TOKEN="$(openssl rand -hex 32)"
sudo install -m 600 -o root -g root /dev/null /etc/viewpie/restart.env
printf 'VIEWPIE_RESTART_TOKEN=%s\n' "$TOKEN" | sudo tee /etc/viewpie/restart.env >/dev/null
```

Add the following to the `[Service]` section of
`/etc/systemd/system/viewpie.service`:

```ini
EnvironmentFile=/etc/viewpie/restart.env
```

Allow the service account to run exactly one command without a password.
Replace `shnyak` only if the systemd service uses a different `User=`:

```bash
echo 'shnyak ALL=(root) NOPASSWD: /usr/sbin/reboot' | sudo tee /etc/sudoers.d/viewpie-reboot
sudo chmod 440 /etc/sudoers.d/viewpie-reboot
sudo visudo -cf /etc/sudoers.d/viewpie-reboot
```

Then validate and restart Viewpie:

```bash
sudo systemctl daemon-reload
sudo systemctl restart viewpie
```

The integration's **Перезагрузить Raspberry Pi** button appears as available
only when the endpoint reports that a token is configured and the integration
has a non-empty API key. The button asks for confirmation before sending the
restart request.
