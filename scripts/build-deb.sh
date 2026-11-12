#!/bin/sh
# Cross-build Viewpie Debian packages for Raspberry Pi OS Bookworm without Docker.
#
#   scripts/build-deb.sh armhf
#   scripts/build-deb.sh arm64
#
# Run on an amd64 Debian/Ubuntu host with sudo, rustup and cargo installed.
# Target libraries and cross toolchains come from Debian Bookworm so the
# resulting packages retain the Bookworm ABI used by Raspberry Pi OS.
set -eu

cd "$(dirname "$0")/.."
targets="${*:-armhf arm64}"
mkdir -p dist

if [ "$(dpkg --print-architecture)" != amd64 ]; then
    echo "build host must be amd64; got $(dpkg --print-architecture)" >&2
    exit 2
fi
command -v cargo >/dev/null 2>&1 || { echo "cargo is required" >&2; exit 2; }
command -v rustup >/dev/null 2>&1 || { echo "rustup is required" >&2; exit 2; }

for t in $targets; do
    case "$t" in
        armhf) GNU=arm-linux-gnueabihf; RUST=armv7-unknown-linux-gnueabihf ;;
        arm64) GNU=aarch64-linux-gnu; RUST=aarch64-unknown-linux-gnu ;;
        *) echo "unknown architecture $t (expected armhf or arm64)" >&2; exit 2 ;;
    esac

    echo "=== preparing Debian Bookworm cross toolchain for $t ==="
    sudo apt-get update -o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30
    sudo apt-get install -y --no-install-recommends debian-archive-keyring
    # Ubuntu's runner sources must remain host-only. Otherwise apt tries to
    # fetch armhf/arm64 indices from Ubuntu security mirrors while resolving
    # the Debian Bookworm target sysroot.
    if [ -f /etc/apt/sources.list.d/ubuntu.sources ]; then
        sudo sed -i '/^Types:/a Architectures: amd64' /etc/apt/sources.list.d/ubuntu.sources
    fi
    sudo dpkg --add-architecture "$t"
    sudo tee /etc/apt/sources.list.d/viewpie-bookworm-cross.list >/dev/null <<EOF
deb [arch=amd64,$t signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] http://deb.debian.org/debian bookworm main
EOF
    sudo tee /etc/apt/preferences.d/viewpie-bookworm-cross >/dev/null <<'EOF'
Package: *
Pin: release n=bookworm
Pin-Priority: 100
EOF
    sudo apt-get update -o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30
    sudo apt-get install -y --no-install-recommends \
        debhelper dpkg-dev fakeroot pkg-config libclang-dev clang ca-certificates
    sudo apt-get install -y --no-install-recommends -t bookworm \
        "crossbuild-essential-$t" \
        "libavcodec-dev:$t" \
        "libavformat-dev:$t" \
        "libavutil-dev:$t" \
        "libssl-dev:$t"

    rustup target add "$RUST"
    target_env=$(printf '%s' "$RUST" | tr '[:lower:]-' '[:upper:]_')
    cc_env=$(printf '%s' "$RUST" | tr '-' '_')
    export "CARGO_TARGET_${target_env}_LINKER=$GNU-gcc"
    export "CC_${cc_env}=$GNU-gcc"
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_LIBDIR="/usr/lib/$GNU/pkgconfig:/usr/share/pkgconfig"
    export PKG_CONFIG_SYSROOT_DIR=/
    export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=/ -I/usr/include/$GNU"
    export VIEWPIE_RUST_TARGET="$RUST"
    export VIEWPIE_BUILD_DATE="${VIEWPIE_BUILD_DATE:-unknown}"

    echo "=== building Debian package for $t without Docker ==="
    rm -f ../viewpie_*_"$t".deb
    dpkg-buildpackage -us -uc -b -d -a"$t"
    cp ../viewpie_*_"$t".deb dist/
done

ls -la dist/*.deb
