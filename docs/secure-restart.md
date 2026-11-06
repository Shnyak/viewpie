# Protected Raspberry Pi restart

Viewpie exposes POST /api/restart. The endpoint requires the dedicated
VIEWPIE_RESTART_TOKEN bearer token and verifies that the running service
has Linux CAP_SYS_BOOT before accepting the request.

The Debian package grants only CAP_SYS_BOOT to the unprivileged viewpie
service using systemd CapabilityBoundingSet and AmbientCapabilities.
The service continues to run as user viewpie with NoNewPrivileges=yes.
No sudoers entry, root shell, or general-purpose privilege escalation is used.

## Configure the token on Raspberry Pi

Generate a cryptographically random 256-bit token and store it in a root-only
environment file. Run this after installing Viewpie 0.2.10-fork or newer:

```bash
TOKEN="$(openssl rand -hex 32)"
sudo install -d -m 0755 /etc/viewpie
sudo install -m 600 -o root -g root /dev/null /etc/viewpie/restart.env
printf "VIEWPIE_RESTART_TOKEN=%s\n" "$TOKEN" | sudo tee /etc/viewpie/restart.env >/dev/null
printf "\nСохраните этот токен и введите его в настройках интеграции Viewpie:\n%s\n" "$TOKEN"
unset TOKEN
sudo systemctl daemon-reload
sudo systemctl restart viewpie
```

systemd reads the environment file as the system manager, so it can remain
owned by root and mode 0600; the Viewpie service account does not need direct
filesystem access to the secret.

## Configure Home Assistant

Open Settings → Devices & services → Viewpie → Configure and paste the same
token into Restart token. Save the options. The integration sends the token
only as Authorization: Bearer <token> to POST /api/restart.

The restart_available field in GET /api/status becomes true only when the
token is non-empty and the process has CAP_SYS_BOOT. The Home Assistant button
additionally requires the matching token in integration options and asks for
confirmation before rebooting.

Never publish the token or include it in diagnostic output.
