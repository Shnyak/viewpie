#!/bin/sh
# Build Viewpie Debian packages for Raspberry Pi OS Bookworm without Docker.
# A Debian Bookworm amd64 chroot provides the target ABI; the ARM binary is
# cross-compiled inside it, without emulation and without modifying the host.
#
#   scripts/build-deb.sh armhf
#   scripts/build-deb.sh arm64
set -eu

cd "$(dirname "$0")/.."
targets="${*:-armhf arm64}"
mkdir -p dist

if [ "$(dpkg --print-architecture)" != amd64 ]; then
    echo "build host must be amd64; got $(dpkg --print-architecture)" >&2
    exit 2
fi

sudo apt-get update -o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30
sudo apt-get install -y --no-install-recommends debootstrap debian-archive-keyring ca-certificates

ROOT=""
cleanup() {
    if [ -n "$ROOT" ]; then
        sudo umount "$ROOT/proc" 2>/dev/null || true
        sudo rm -rf "$ROOT"
    fi
}
trap cleanup EXIT HUP INT TERM

for t in $targets; do
    case "$t" in
        armhf) GNU=arm-linux-gnueabihf; RUST=armv7-unknown-linux-gnueabihf ;;
        arm64) GNU=aarch64-linux-gnu; RUST=aarch64-unknown-linux-gnu ;;
        *) echo "unknown architecture $t (expected armhf or arm64)" >&2; exit 2 ;;
    esac

    ROOT=$(mktemp -d "/tmp/viewpie-bookworm-$t.XXXXXX")
    echo "=== creating Debian Bookworm build root for $t ==="
    sudo debootstrap --variant=minbase --arch=amd64 \
        --keyring=/usr/share/keyrings/debian-archive-keyring.gpg \
        bookworm "$ROOT" https://deb.debian.org/debian
    sudo rm -f "$ROOT/etc/resolv.conf"
    sudo cp /etc/resolv.conf "$ROOT/etc/resolv.conf"
    sudo mkdir -p "$ROOT/src" "$ROOT/out"
    sudo cp -a . "$ROOT/src/"
    sudo mount -t proc proc "$ROOT/proc"

    echo "=== cross-building $t inside Debian Bookworm chroot (no Docker) ==="
    sudo chroot "$ROOT" /usr/bin/env \
        ARCH="$t" GNU="$GNU" RUST="$RUST" \
        VIEWPIE_BUILD_DATE="${VIEWPIE_BUILD_DATE:-unknown}" \
        /bin/sh -euc '
            export DEBIAN_FRONTEND=noninteractive
            dpkg --add-architecture "$ARCH"
            apt-get update
            apt-get install -y --no-install-recommends \
                ca-certificates curl debhelper dpkg-dev fakeroot pkg-config \
                crossbuild-essential-"$ARCH" \
                "libavcodec-dev:$ARCH" \
                "libavformat-dev:$ARCH" \
                "libavutil-dev:$ARCH" \
                "libssl-dev:$ARCH" \
                libclang-dev clang

            curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
            sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain stable --target "$RUST"
            export PATH=/root/.cargo/bin:$PATH
            target_env=$(printf "%s" "$RUST" | tr "[:lower:]-" "[:upper:]_")
            cc_env=$(printf "%s" "$RUST" | tr "-" "_")
            export "CARGO_TARGET_${target_env}_LINKER=$GNU-gcc"
            export "CC_${cc_env}=$GNU-gcc"
            export PKG_CONFIG_ALLOW_CROSS=1
            export PKG_CONFIG_LIBDIR="/usr/lib/$GNU/pkgconfig:/usr/share/pkgconfig"
            export PKG_CONFIG_SYSROOT_DIR=/
            export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=/ -I/usr/include/$GNU"
            export VIEWPIE_RUST_TARGET="$RUST"

            cd /src
            dpkg-buildpackage -us -uc -b -d -a"$ARCH"
            cp /viewpie_*_"$ARCH".deb /out/
        '

    sudo cp "$ROOT"/out/viewpie_*_"$t".deb dist/
    cleanup
    ROOT=""
done

ls -la dist/*.deb
