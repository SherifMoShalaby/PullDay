#!/bin/sh
# Checks a .deb from package-deb.sh: the control fields, exactly four files with their modes, a valid .desktop.
# Usage: check-deb.sh <PullDay-<version>-amd64.deb>  (needs dpkg-deb and desktop-file-validate)
set -eu
deb=$1
fail() { echo "check-deb: $*" >&2; exit 1; }
field() { dpkg-deb -f "$deb" "$1"; }

version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$(dirname "$0")/Cargo.toml" | head -n 1)
[ "$(basename "$deb")" = "PullDay-$version-amd64.deb" ] || fail "file name isn't PullDay-$version-amd64.deb"
[ "$(field Package)" = pullday ] || fail "Package"
[ "$(field Version)" = "$version" ] || fail "Version isn't $version"
[ "$(field Architecture)" = amd64 ] || fail "Architecture"
[ "$(field Maintainer)" = "Sherif Shalaby <40992542+SherifMoShalaby@users.noreply.github.com>" ] || fail "Maintainer"
[ "$(field Recommends)" = "curl, git, xdg-utils" ] || fail "Recommends"
depends=$(field Depends)
case $depends in
    *shlibs:* | "") fail "Depends: '$depends'" ;;
    *libc6*libwebkit2gtk-4.1-0*) ;;
    *) fail "Depends lacks libc6 or libwebkit2gtk-4.1-0: '$depends'" ;;
esac

files=$(dpkg-deb -c "$deb" | awk '$1 !~ /^d/ { print $1, $2, $6 }')
expected='-rwxr-xr-x root/root ./usr/bin/pullday
-rw-r--r-- root/root ./usr/share/applications/pullday.desktop
-rw-r--r-- root/root ./usr/share/doc/pullday/copyright
-rw-r--r-- root/root ./usr/share/icons/hicolor/128x128/apps/pullday.png'
[ "$files" = "$expected" ] || fail "files:
$files"
dirs=$(dpkg-deb -c "$deb" | awk '$1 ~ /^d/ && ($1 != "drwxr-xr-x" || $2 != "root/root")')
[ -z "$dirs" ] || fail "folders not 0755 root:
$dirs"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
dpkg-deb --fsys-tarfile "$deb" | tar -xO ./usr/share/applications/pullday.desktop >"$tmp/pullday.desktop"
desktop-file-validate "$tmp/pullday.desktop"
echo "check-deb: $deb ok"
