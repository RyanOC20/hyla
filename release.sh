#!/usr/bin/env bash
#
# release.sh — build, sign, and package hyla into a distributable macOS bundle.
#
# Produces, from a clean release build:
#   target/release/bundle/osx/Hyla.app   — ad-hoc signed, fully sealed
#   target/release/bundle/dmg/Hyla.dmg   — compressed, drag-to-install, signed
#
# The steps run in a fixed order because it matters: the app must be signed
# BEFORE the DMG is built from it, or the DMG ships an unsigned app.
#
# Usage:
#   ./release.sh                 # ad-hoc sign (personal use on this Mac)
#   ./release.sh "Developer ID Application: Your Name (TEAMID)"
#                                # sign with a real identity for distribution
#
# Requires: cargo-bundle (cargo install cargo-bundle).

set -euo pipefail

cd "$(dirname "$0")"

APP="target/release/bundle/osx/Hyla.app"
DMG="target/release/bundle/dmg/Hyla.dmg"
IDENTITY="${1:--}"   # default "-" = ad-hoc

say() { printf '\033[1;32m==>\033[0m %s\n' "$1"; }

# --- preflight -------------------------------------------------------------
if ! command -v cargo-bundle >/dev/null 2>&1; then
  echo "error: cargo-bundle not found. Install with: cargo install cargo-bundle" >&2
  exit 1
fi

# --- quality gates ---------------------------------------------------------
say "Checking formatting"
cargo fmt --check

say "Running clippy"
cargo clippy --release --all-targets -- -D warnings

# --- build the bundle ------------------------------------------------------
# cargo bundle also emits a DMG, but it wraps the app BEFORE we re-sign it, so
# we discard that DMG and build our own from the signed app below.
say "Building release bundle"
cargo bundle --release
rm -f "$DMG"

# --- sign the app (seal the whole bundle) ----------------------------------
say "Signing app ($([ "$IDENTITY" = "-" ] && echo "ad-hoc" || echo "$IDENTITY"))"
codesign --force --deep --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

# --- package the signed app into a fresh DMG -------------------------------
say "Packaging DMG"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
mkdir -p "$(dirname "$DMG")"
hdiutil create -volname "Hyla" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
codesign --force --sign "$IDENTITY" "$DMG"

# --- verify the app INSIDE the DMG (end-to-end) ----------------------------
say "Verifying packaged app"
MNT="$(mktemp -d)"
hdiutil attach "$DMG" -mountpoint "$MNT" -nobrowse -quiet
codesign --verify --deep --strict "$MNT/Hyla.app"
hdiutil detach "$MNT" -quiet
rmdir "$MNT" 2>/dev/null || true

say "Done"
echo "  app: $APP"
echo "  dmg: $DMG  ($(du -h "$DMG" | cut -f1))"
