#!/usr/bin/env bash
set -euo pipefail

VERSION="0.2.35-fork"
TAG="v${VERSION}"
REPOSITORY="Shnyak/viewpie"
BASE_URL="https://github.com/${REPOSITORY}/releases/download/${TAG}"

if [ "$(id -u)" -ne 0 ]; then
  echo "Run this bootstrap as root, for example: curl ... | sudo bash" >&2
  exit 1
fi

ARCH="$(dpkg --print-architecture)"
case "$ARCH" in
  armhf|arm64) ;;
  *)
    echo "Unsupported architecture: ${ARCH} (expected armhf or arm64)" >&2
    exit 1
    ;;
esac

PACKAGE="viewpie_${VERSION}-1_${ARCH}.deb"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

download() {
  local url="$1"
  local destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --retry 3 --silent --show-error "$url" --output "$destination"
  elif command -v wget >/dev/null 2>&1; then
    wget --quiet --output-document="$destination" "$url"
  else
    echo "Neither curl nor wget is installed" >&2
    exit 1
  fi
}

echo "=== Viewpie one-time updater bootstrap ==="
echo "Target: ${VERSION}"
echo "Architecture: ${ARCH}"
echo "Package: ${PACKAGE}"

echo
echo "=== Download SHA256 manifest ==="
download "${BASE_URL}/SHA256SUMS" "${TMP_DIR}/SHA256SUMS"

echo
echo "=== Download Debian package ==="
download "${BASE_URL}/${PACKAGE}" "${TMP_DIR}/${PACKAGE}"

echo
echo "=== Verify SHA-256 ==="
awk -v name="${PACKAGE}" '$2 == name { print $1 "  " $2 }' \
  "${TMP_DIR}/SHA256SUMS" > "${TMP_DIR}/selected.sha256"
if [ "$(wc -l < "${TMP_DIR}/selected.sha256")" -ne 1 ]; then
  echo "SHA256SUMS does not contain exactly one entry for ${PACKAGE}" >&2
  exit 1
fi
(
  cd "${TMP_DIR}"
  sha256sum --check selected.sha256
)

read_deb_field() {
  local field="$1"
  local value
  value="$(dpkg-deb -f "${TMP_DIR}/${PACKAGE}" "$field")"
  value="${value#"${field}:"}"
  value="$(printf '%s' "$value" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
  printf '%s' "$value"
}

echo
echo "=== Verify Debian package metadata ==="
PACKAGE_NAME="$(read_deb_field Package)"
PACKAGE_VERSION="$(read_deb_field Version)"
PACKAGE_ARCH="$(read_deb_field Architecture)"
printf 'Package: %s\nVersion: %s\nArchitecture: %s\n' \
  "$PACKAGE_NAME" "$PACKAGE_VERSION" "$PACKAGE_ARCH"

if [ "$PACKAGE_NAME" != "viewpie" ] \
  || [ "$PACKAGE_VERSION" != "${VERSION}-1" ] \
  || [ "$PACKAGE_ARCH" != "$ARCH" ]; then
  echo "Downloaded package identity does not match the selected release" >&2
  exit 1
fi

echo
echo "=== Install runtime dependencies ==="
apt-get update
apt-get install -y --no-install-recommends ffmpeg fonts-dejavu-core

echo
echo "=== Install package ==="
dpkg -i "${TMP_DIR}/${PACKAGE}"

echo
echo "=== Verify installed version ==="
INSTALLED_VERSION="$(dpkg-query -W -f='${Version}' viewpie)"
if [ "$INSTALLED_VERSION" != "${VERSION}-1" ]; then
  echo "Installed version mismatch: expected ${VERSION}-1, got ${INSTALLED_VERSION}" >&2
  exit 1
fi

echo
echo "SUCCESS: Viewpie ${VERSION} (${ARCH}) is installed."
echo "The package is installed. Raspberry Pi will reboot automatically in 3 seconds."
sleep 3
systemctl reboot
