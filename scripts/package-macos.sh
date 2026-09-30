#!/usr/bin/env bash
# Local, manual packaging: release build for Apple silicon → signed oh-my-clear.app
# (daemon in Contents/Helpers, omc_ipc::layout / ADR 0020) → signed DMG → optional notarization.
#
# Usage: scripts/package-macos.sh [--no-notarize] [--skip-build]
#
# Environment (defaults suit the local developer machine):
#   OMC_SIGN_IDENTITY  codesign identity   (default: first "Developer ID Application" in keychain)
#   OMC_ASC_KEY        App Store Connect API key .p8
#   OMC_ASC_KEY_ID     API key id
#   OMC_ASC_ISSUER     API issuer id
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

NOTARIZE=1
BUILD=1
for arg in "$@"; do
  case "$arg" in
    --no-notarize) NOTARIZE=0 ;;
    --skip-build) BUILD=0 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

CRED_DIR="/Users/zero/development/apple开发者信息"
ASC_KEY="${OMC_ASC_KEY:-$CRED_DIR/AuthKey_2Y22LG8WUS.p8}"
ASC_KEY_ID="${OMC_ASC_KEY_ID:-2Y22LG8WUS}"
ASC_ISSUER="${OMC_ASC_ISSUER:-1e0d336d-c7e5-4128-9267-0543d6fbf8ca}"

TARGET=aarch64-apple-darwin
APP=oh-my-clear
DAEMON=oh-my-clear-daemon
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN_DIR="$TARGET_DIR/$TARGET/release"
OUT="$ROOT/dist"
STAGE="$OUT/stage"
BUNDLE="$STAGE/$APP.app"
DMG="$OUT/$APP-$VERSION-arm64.dmg"

IDENTITY="${OMC_SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
  | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -n1)}"
[[ -n "$IDENTITY" ]] || { echo "no Developer ID Application identity in keychain" >&2; exit 1; }

step() { printf '\n==> %s\n' "$*"; }

if (( BUILD )); then
  step "Building release ($TARGET)"
  rustup target add "$TARGET" >/dev/null
  cargo build --release --locked --target "$TARGET" -p "$APP" -p "$DAEMON"
fi

step "Assembling $BUNDLE"
rm -rf "$STAGE"
fill_bundle() { # <contents dir> <package>
  local contents="$1" pkg="$2"
  mkdir -p "$contents/MacOS" "$contents/Resources"
  sed "s/@VERSION@/$VERSION/g" "apps/$pkg/resources/macos/Info.plist" >"$contents/Info.plist"
  cp "apps/$APP/resources/macos/AppIcon.icns" "$contents/Resources/AppIcon.icns"
  cp "$BIN_DIR/$pkg" "$contents/MacOS/$pkg"
}
HELPER="$BUNDLE/Contents/Helpers/$DAEMON.app"
fill_bundle "$BUNDLE/Contents" "$APP"
fill_bundle "$HELPER/Contents" "$DAEMON"

step "Signing with: $IDENTITY"
# Inside-out: the helper first, then the outer app (its seal covers the signed helper).
sign() { codesign --force --timestamp --options runtime --sign "$IDENTITY" "$@"; }
sign "$HELPER"
sign "$BUNDLE"
codesign --verify --deep --strict --verbose=2 "$BUNDLE"

step "Creating $DMG"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "$APP $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
sign "$DMG"

if (( NOTARIZE )); then
  step "Notarizing (App Store Connect key $ASC_KEY_ID)"
  xcrun notarytool submit "$DMG" --key "$ASC_KEY" --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER" --wait
  xcrun stapler staple "$DMG"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
fi

rm -rf "$STAGE"
step "Done: $DMG"
