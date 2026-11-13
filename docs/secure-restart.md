# Protected Viewpie HTTP API and Raspberry Pi restart

The Debian package creates a cryptographically random 256-bit service token on
first installation and stores it in the root-only `/etc/viewpie/restart.env`
file. The token is printed once by the package installer so it can be entered
in Home Assistant. Package upgrades preserve the existing token. Older
installations that used `/etc/viewpie/viewpie.env` are migrated without
printing the secret.

**The same Bearer token protects every Viewpie HTTP API route**, not only
restart and update installation. This includes status, camera metadata,
camera snapshots, panels, settings, display controls and update discovery.
Requests without the token or with an incorrect token receive HTTP 401 (or HTTP 503 if the service token itself is not configured). CORS
is not an authentication mechanism. The API currently uses plain HTTP, so Bearer tokens are not encrypted in transit. Restrict port 8080 to a trusted LAN and do not expose it directly to the internet; use a TLS reverse proxy when clients cross an untrusted network.

The Debian service runs as the unprivileged `viewpie` account and receives only
`CAP_SYS_BOOT` through systemd. No sudoers entry or general-purpose privilege
escalation is used for reboot.

## Home Assistant

During initial integration setup, paste the service token printed by the
package installer into **Viewpie service token** (or the API token field).
For an existing integration, open Settings → Devices & services → Viewpie →
Configure and enter the same token. The integration sends it as a Bearer
header for all API requests. If the token is changed in integration options,
the existing API client updates it without restarting Home Assistant.

For direct REST clients, use `Authorization: Bearer <token>`. Keep the token
in a secrets store, never in a URL, dashboard source, or diagnostic log.

## Verified updates from Home Assistant

The system management card can check the latest GitHub release for the
Raspberry Pi architecture. Installation is enabled only when a newer matching
package and a SHA-256 digest are available. The user must start installation
manually and confirm it in the card.

The package installer is a separate root-owned, systemd socket-activated
helper. The Viewpie API can request only the fixed `INSTALL` operation; it
cannot pass an arbitrary URL, file path, shell command, or package name. The
helper fetches the latest release directly from the fixed GitHub repository,
checks the architecture, verifies the GitHub-published SHA-256 digest,
validates the Debian package identity, and installs the local package through
APT so dependencies are resolved as part of the transaction.

The helper runs package operations noninteractively and enforces a real
process/output deadline. It refreshes package metadata, installs runtime
prerequisites only when absent, and pins the exact installed FFmpeg package version in the APT transaction:
Viewpie itself can be upgraded, but FFmpeg cannot be silently replaced as a
side effect of that update.
The FFmpeg package version is recorded in the update journal. If the new
Viewpie package requires a dependency upgrade, the transaction fails rather
than changing the existing FFmpeg/runtime stack without an explicit decision.

After a successful package installation, the helper requests a full Raspberry
Pi reboot. This is intentional: it ensures the running renderer and loaded
libraries are fully replaced. The update journal reports success before the
reboot is requested.

Never publish the token or include it in diagnostic output.
