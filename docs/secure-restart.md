# Protected Raspberry Pi restart

Viewpie exposes a token-protected `POST /api/restart` endpoint. The Debian
package creates a cryptographically random 256-bit token on first installation
and stores it in the root-only `/etc/viewpie/restart.env` file. The token is
printed once by the package installer so it can be entered in Home Assistant.
Package upgrades preserve the existing token. Older installations that used
`/etc/viewpie/viewpie.env` are migrated without printing the secret.

The Debian service runs as the unprivileged `viewpie` account and receives only
`CAP_SYS_BOOT` through systemd. No sudoers entry or general-purpose privilege
escalation is used for reboot.

## Home Assistant

During initial integration setup, paste the one-time token printed by the
package installer into the optional **Restart / update token** field. For an
existing integration, open Settings → Devices & services → Viewpie → Configure
and enter the same token. The token is sent only as a Bearer authorization
header to the protected restart and update endpoints.

## Verified updates from Home Assistant

The system management card can check the latest GitHub release for the
Raspberry Pi architecture. Installation is enabled only when a newer matching
package and a SHA-256 digest are available. The user must start installation
manually and confirm it in the card.

The package installer is a separate root-owned, systemd socket-activated helper.
The Viewpie API can request only the fixed `INSTALL` operation; it cannot pass
an arbitrary URL, file path, shell command, or package name. The helper fetches
the latest release directly from the fixed GitHub repository, checks the
architecture, verifies the GitHub-published SHA-256 digest, validates the
Debian package identity, and only then runs `dpkg -i`.

The helper logs installation errors to the system journal. A package update
restarts the Viewpie service through Debian's normal package hooks. It does not
automatically reboot the Raspberry Pi.

Never publish the token or include it in diagnostic output.
