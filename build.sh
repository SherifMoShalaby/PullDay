#!/usr/bin/env bash
# Builds PullDay.app. Needs the Xcode Command Line Tools (xcode-select --install).
#   ./build.sh             build into ./build/PullDay.app
#   ./build.sh --install   build, copy to /Applications and launch
#   ./build.sh --test      compile and run Tests/SelfTest.swift (no app build)
#   ./build.sh --dmg       build, then package build/PullDay.dmg
#     SIGN_IDENTITY="Developer ID Application: …"  sign for distribution (hardened runtime)
#     NOTARY_PROFILE=<notarytool keychain profile>  also notarize and staple the DMG
set -euo pipefail
cd "$(dirname "$0")"

case "${1:-}" in
  ""|--test|--install|--dmg) ;;
  *) echo "Unknown option: $1. Use one of --test, --install or --dmg."; exit 1 ;;
esac

if ! command -v swiftc >/dev/null 2>&1; then
  echo "swiftc not found. Install the Xcode Command Line Tools first:  xcode-select --install"
  exit 1
fi

if [[ -n "${NOTARY_PROFILE:-}" && -z "${SIGN_IDENTITY:-}" ]]; then
  echo "NOTARY_PROFILE needs SIGN_IDENTITY: Apple only notarizes apps signed with a Developer ID."
  exit 1
fi

if [[ "${1:-}" == "--test" ]]; then
  mkdir -p build
  # App.swift has the app's @main, so the self-test compiles the logic files with its own.
  swiftc -swift-version 5 -parse-as-library -target "$(uname -m)-apple-macos13.0" \
    Sources/Scanner.swift Sources/Shell.swift Sources/Models.swift Sources/Prefs.swift Tests/SelfTest.swift \
    -o build/selftest
  exec build/selftest
fi

APP="build/PullDay.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

echo "Compiling…"
build_arch() {
  swiftc -O -swift-version 5 -parse-as-library \
    -target "$1-apple-macos13.0" \
    Sources/*.swift -o "$2"
}
build_arch arm64 build/PullDay-arm64
build_arch x86_64 build/PullDay-x86_64
lipo -create build/PullDay-arm64 build/PullDay-x86_64 -output "$APP/Contents/MacOS/PullDay"
rm -f build/PullDay-arm64 build/PullDay-x86_64

cp Info.plist "$APP/Contents/Info.plist"
cp Resources/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

if [[ -n "${SIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
else
  # Ad-hoc signature so macOS will run it and allow notifications / login item.
  codesign --force --sign - "$APP"
fi
echo "Built $APP"

if [[ "${1:-}" == "--dmg" ]]; then
  DMG="build/PullDay.dmg"
  STAGE="build/dmg"
  rm -rf "$STAGE" "$DMG"
  mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -quiet -volname PullDay -srcfolder "$STAGE" -format UDZO "$DMG"
  rm -rf "$STAGE"
  if [[ -n "${SIGN_IDENTITY:-}" ]]; then
    codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"
  fi
  if [[ -n "${NOTARY_PROFILE:-}" ]]; then
    xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
    echo "Built $DMG (signed and notarized)"
  elif [[ -n "${SIGN_IDENTITY:-}" ]]; then
    echo "Built $DMG (signed, not notarized; set NOTARY_PROFILE to notarize)"
  else
    echo "Built $DMG (ad-hoc signed, for local use only; set SIGN_IDENTITY and NOTARY_PROFILE to sign and notarize)"
  fi
fi

if [[ "${1:-}" == "--install" ]]; then
  pkill -x PullDay 2>/dev/null || true
  rm -rf /Applications/PullDay.app
  cp -R "$APP" /Applications/
  open /Applications/PullDay.app
  echo "Installed to /Applications and launched. Look for the barbell icon in your menu bar."
fi
