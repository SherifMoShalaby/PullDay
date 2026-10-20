#!/bin/sh
# Builds PullDay-<version>-amd64.deb into <out-dir> (Linux only; needs dpkg-dev, no root):
# a release build with the embedded ui/, four staged files, Depends from dpkg-shlibdeps.
set -eu
umask 022 # staged files 0644/0755 whatever the caller's umask

[ "$(dpkg --print-architecture)" = amd64 ] || { echo "PullDay's .deb is amd64 only" >&2; exit 1; }
[ $# -eq 1 ] || { echo "usage: $0 <out-dir>" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$1"
out=$(cd "$1" && pwd)
version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$here/Cargo.toml" | head -n 1)
[ -n "$version" ] || { echo "no version in $here/Cargo.toml" >&2; exit 1; }

# custom-protocol is what tauri-cli turns on for production builds: assets from the binary, no dev server.
"${CARGO:-cargo}" build --release --locked -p pullday-linux --features tauri/custom-protocol \
    --manifest-path "$here/../Cargo.toml"
binary=${CARGO_TARGET_DIR:-$here/../target}/release/pullday
[ "$("$binary" --version)" = "PullDay $version" ] || { echo "stale $binary" >&2; exit 1; }

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root=$stage/root
install -D -s -m 0755 "$binary" "$root/usr/bin/pullday"
install -D -m 0644 "$here/pullday.desktop" "$root/usr/share/applications/pullday.desktop"
install -D -m 0644 "$here/icons/icon.png" "$root/usr/share/icons/hicolor/128x128/apps/pullday.png"
install -D -m 0644 "$here/../../LICENSE" "$root/usr/share/doc/pullday/copyright"

# dpkg-shlibdeps wants a debian/control to read and a DEBIAN folder at the package root; it prints
# "shlibs:Depends=<list>".
mkdir "$stage/debian" "$root/DEBIAN"
printf 'Source: pullday\n\nPackage: pullday\nArchitecture: amd64\n' >"$stage/debian/control"
depends=$(cd "$stage" && dpkg-shlibdeps -O root/usr/bin/pullday | sed -n 's/^shlibs:Depends=//p')
[ -n "$depends" ] || { echo "dpkg-shlibdeps found no dependencies" >&2; exit 1; }

cat >"$root/DEBIAN/control" <<EOF
Package: pullday
Version: $version
Architecture: amd64
Section: utils
Priority: optional
Maintainer: Sherif Shalaby <40992542+SherifMoShalaby@users.noreply.github.com>
Installed-Size: $(du -sk "$root/usr" | cut -f 1)
Depends: $depends
Recommends: curl, git, xdg-utils
Homepage: https://github.com/SherifMoShalaby/PullDay
Description: Keeps your AI coding agents' skills and plugins up to date
 A tray app that checks the skills, plugins and extensions of Claude Code, Codex, Gemini CLI and others for updates.
EOF

dpkg-deb --build --root-owner-group "$root" "$out/PullDay-$version-amd64.deb"
