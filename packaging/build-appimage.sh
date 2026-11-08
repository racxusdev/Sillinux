#!/bin/sh
# Build the Sillinux AppImage.
#
# Produces dist/Sillinux-x86_64.AppImage, a Type 2 AppImage whose binary
# targets glibc 2.17 (Ubuntu 14.04+/Debian 8+/CentOS 7+ era and newer).
# libusb is built from source and linked statically, without udev, so the
# AppDir needs no bundled libraries at all.
#
# Requirements:
#   - cargo-zigbuild (cargo install cargo-zigbuild) and zig 0.13/0.14 on PATH
#   - appimagetool: set APPIMAGETOOL=/path/to/appimagetool to override, or let
#     this script download the official binary into packaging/tools/ once.
#
# Without FUSE on the host, everything still works: the script itself runs
# appimagetool with --appimage-extract-and-run, and the produced AppImage can
# be started with --appimage-extract-and-run too.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

ARCH="${ARCH:-x86_64}"
TARGET="x86_64-unknown-linux-gnu.2.17"
VERSION=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)

if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    echo "error: cargo-zigbuild is required (cargo install cargo-zigbuild)" >&2
    exit 1
fi

echo "==> version $VERSION, target $TARGET"

# An empty pkg-config search path keeps the vendored libusb build free of
# udev (and of the host's glibc headers), so the binary links only glibc.
export PKG_CONFIG_LIBDIR=/nonexistent
export PKG_CONFIG_PATH=/nonexistent

cargo zigbuild --release --target "$TARGET" \
    -p silhouette-ui --features gui,vendored-usb --bin silhouette-gui
cargo zigbuild --release --target "$TARGET" \
    -p silhouette-app --features vendored-usb --bin silhouette

BUILD="$ROOT/target/x86_64-unknown-linux-gnu/release"
APPDIR="$ROOT/AppDir"

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
         "$APPDIR/usr/share/metainfo"

install -m755 "$BUILD/silhouette-gui" "$APPDIR/usr/bin/sillinux"
install -m755 "$BUILD/silhouette"     "$APPDIR/usr/bin/silhouette"
install -m644 packaging/sillinux.desktop         "$APPDIR/sillinux.desktop"
install -m644 packaging/sillinux.desktop         "$APPDIR/usr/share/applications/sillinux.desktop"
install -m644 packaging/sillinux.png             "$APPDIR/sillinux.png"
install -m644 packaging/sillinux.png             "$APPDIR/usr/share/icons/hicolor/256x256/apps/sillinux.png"
install -m644 packaging/sillinux.metainfo.xml    "$APPDIR/usr/share/metainfo/sillinux.metainfo.xml"
ln -sf sillinux.png "$APPDIR/.DirIcon"

cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE=$(dirname "$(readlink -f "$0")")
export LD_LIBRARY_PATH="$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/sillinux" "$@"
EOF
chmod +x "$APPDIR/AppRun"

if [ -z "${APPIMAGETOOL:-}" ]; then
    APPIMAGETOOL="$ROOT/packaging/tools/appimagetool-${ARCH}.AppImage"
    if [ ! -x "$APPIMAGETOOL" ]; then
        echo "==> downloading appimagetool"
        mkdir -p "$(dirname "$APPIMAGETOOL")"
        curl -fL --retry 3 \
            -o "$APPIMAGETOOL" \
            "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
        chmod +x "$APPIMAGETOOL"
    fi
fi

# The current continuous appimagetool mishandles --appimage-extract-and-run
# (fails with an empty-filename error), so extract it once and run the inner
# AppRun. This also removes any FUSE dependency on the build host.
EXTRACT="$ROOT/packaging/tools/appimagetool.squashfs"
if [ ! -x "$EXTRACT/AppRun" ]; then
    rm -rf "$EXTRACT" "$ROOT/squashfs-root"
    "$APPIMAGETOOL" --appimage-extract >/dev/null
    mv "$ROOT/squashfs-root" "$EXTRACT"
fi

mkdir -p dist
# appimagetool (continuous build 368+) resolves relative paths against its own
# working directory after chdir, so absolute paths are required here.
VERSION="$VERSION" ARCH="$ARCH" \
    "$EXTRACT/AppRun" --no-appstream \
    "$APPDIR" "$ROOT/dist/Sillinux-$ARCH.AppImage"

echo "==> wrote dist/Sillinux-$ARCH.AppImage"
