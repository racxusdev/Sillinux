# packaging

Builds `dist/Sillinux-x86_64.AppImage` (Type 2) with `./build-appimage.sh`.

- The binary targets **glibc 2.17** via `cargo-zigbuild` + Zig, so the same
  AppImage runs on old and new distros alike (Ubuntu 14.04+/Debian 8+/CentOS 7+,
  Arch, Fedora…).
- libusb is compiled from source and linked **statically**, without udev — the
  AppDir carries no bundled libraries; only glibc is required.
- `appimagetool` is downloaded once into `tools/` (git-ignored). Set
  `APPIMAGETOOL=/path/to/appimagetool` to use your own copy.
- No FUSE is needed to build (the script uses `--appimage-extract-and-run`) and
  the resulting AppImage also starts without FUSE via
  `./Sillinux-x86_64.AppImage --appimage-extract-and-run`.

Contents of the AppDir:

```
AppRun                          # sets LD_LIBRARY_PATH, execs usr/bin/sillinux
sillinux.desktop                # desktop entry (also under usr/share/applications)
sillinux.png / .DirIcon         # icon (also under usr/share/icons/hicolor/256x256/apps)
usr/bin/sillinux                # GUI (silhouette-gui)
usr/bin/silhouette              # CLI (inspect, svg, print-sheet, cut-file…)
usr/share/metainfo/…            # AppStream metadata
```

Install the udev rule from `udev/99-silhouette.rules` so the desktop user can
open the cutter without sudo; see the repository README.

Bluetooth needs no host configuration: the GUI opens the serial profile with a
plain RFCOMM socket as the desktop user (no root, no `rfcomm`, no polkit).
