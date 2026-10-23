#!/bin/bash
# Builds prev.app on macOS: the release binary, its icon, and an
# Info.plist that declares the files prev opens, so Finder offers it in
# Open With and hands it files through the app delegate. The app is
# signed ad hoc, which Apple Silicon needs to run it at all, unless
# given a Developer ID (docs/RELEASING.md).
#
#   packaging/macos/bundle.sh [--dmg] [--out DIR]
#
# PREV_PRODUCTION=1 makes a release build, as in CI; without it the app
# is the development build, with its own settings and data.
#
# PREV_SIGN_IDENTITY signs the app, the libraries it carries and the
# disk image with that identity, the hardened runtime and a secure
# timestamp, as notarization asks, from the keychain PREV_SIGN_KEYCHAIN
# when given, unlocked with PREV_SIGN_KEYCHAIN_PASSWORD.
# PREV_NOTARY_KEY, an App Store Connect API key's .p8 file, with
# PREV_NOTARY_KEY_ID and PREV_NOTARY_ISSUER, has the app and the disk
# image notarized and their tickets stapled to them.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
out="$root/target/macos"
dmg=false
while [ $# -gt 0 ]; do
    case "$1" in
        --dmg) dmg=true ;;
        --out) out=$2; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
if [ -n "${PREV_PRODUCTION:-}" ]; then
    name=prev; id=run.prev
else
    name=prev-dev; id=run.prev.dev
fi

# bindgen, for MuPDF's bindings, needs libclang: the Command Line Tools'
# or Xcode's.
if [ -z "${LIBCLANG_PATH:-}" ]; then
    developer=$(xcode-select -p)
    for dir in "$developer/usr/lib" "$developer/Toolchains/XcodeDefault.xctoolchain/usr/lib"; do
        if [ -e "$dir/libclang.dylib" ]; then
            export LIBCLANG_PATH=$dir
            break
        fi
    done
fi
cargo build --release --locked -p prev --manifest-path "$root/Cargo.toml"

app="$out/$name.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Frameworks"
cp "$root/target/release/prev" "$app/Contents/MacOS/prev"

# The icon: each size an .iconset asks for, from the PNGs the Linux
# packages use; 1024 is scaled up from 512.
icons="$root/data/icons/hicolor"
iconset=$(mktemp -d)/prev.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    cp "$icons/${size}x${size}/apps/run.prev.png" "$iconset/icon_${size}x${size}.png"
done
cp "$icons/32x32/apps/run.prev.png" "$iconset/icon_16x16@2x.png"
cp "$icons/64x64/apps/run.prev.png" "$iconset/icon_32x32@2x.png"
cp "$icons/256x256/apps/run.prev.png" "$iconset/icon_128x128@2x.png"
cp "$icons/512x512/apps/run.prev.png" "$iconset/icon_256x256@2x.png"
sips -z 1024 1024 "$icons/512x512/apps/run.prev.png" \
    --out "$iconset/icon_512x512@2x.png" >/dev/null
iconutil -c icns "$iconset" -o "$app/Contents/Resources/prev.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$name</string>
    <key>CFBundleDisplayName</key><string>$name</string>
    <key>CFBundleIdentifier</key><string>$id</string>
    <key>CFBundleExecutable</key><string>prev</string>
    <key>CFBundleIconFile</key><string>prev</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>AGPL-3.0-or-later</string>
    <key>NSLocalNetworkUsageDescription</key><string>The print panel looks for printers on your network. prev itself does not use the network.</string>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>PDF document</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key><array><string>com.adobe.pdf</string></array>
        </dict>
        <dict>
            <key>CFBundleTypeName</key><string>Image</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.image</string>
                <string>public.camera-raw-image</string>
                <string>public.svg-image</string>
            </array>
        </dict>
        <dict>
            <key>CFBundleTypeName</key><string>Markdown document</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key><array><string>net.daringfireball.markdown</string></array>
        </dict>
    </array>
    <key>UTImportedTypeDeclarations</key>
    <array>
        <dict>
            <key>UTTypeIdentifier</key><string>net.daringfireball.markdown</string>
            <key>UTTypeDescription</key><string>Markdown document</string>
            <key>UTTypeConformsTo</key><array><string>public.plain-text</string></array>
            <key>UTTypeTagSpecification</key>
            <dict>
                <key>public.filename-extension</key>
                <array><string>md</string><string>markdown</string></array>
            </dict>
        </dict>
    </array>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

# HEIC and AVIF: the decode-only libheif build-libheif.sh makes, when it
# has been built (the release does); else, for a build on this Mac only,
# Homebrew's, with the libraries it needs.
licenses="$app/Contents/Resources/licenses"
if [ -e "$root/target/libheif/lib/libheif.1.dylib" ]; then
    cp "$root/target/libheif/lib/libheif.1.dylib" "$app/Contents/Frameworks/"
    mkdir -p "$licenses"
    cp "$root"/target/libheif/{COPYING-*,LICENSE-*,PATENTS-*} "$licenses/"
else
    for prefix in /opt/homebrew /usr/local; do
        heif="$prefix/lib/libheif.1.dylib"
        [ -e "$heif" ] || continue
        cp -L "$heif" "$app/Contents/Frameworks/"
        for dependency in $(otool -L "$heif" | awk 'NR > 1 {print $1}' | grep "^$prefix/"); do
            base=$(basename "$dependency")
            cp -L "$dependency" "$app/Contents/Frameworks/$base"
            install_name_tool -change "$dependency" "@loader_path/$base" \
                "$app/Contents/Frameworks/libheif.1.dylib"
        done
        break
    done
fi
if [ -e "$app/Contents/Frameworks/libheif.1.dylib" ]; then
    install_name_tool -id @executable_path/../Frameworks/libheif.1.dylib \
        "$app/Contents/Frameworks/libheif.1.dylib"
fi

# Extended attributes would break the signature's seal.
xattr -cr "$app"
identity=${PREV_SIGN_IDENTITY:--}
sign=(--force --sign "$identity")
if [ "$identity" != - ]; then
    sign+=(--options runtime --timestamp)
fi
if [ -n "${PREV_SIGN_KEYCHAIN:-}" ]; then
    security unlock-keychain -p "${PREV_SIGN_KEYCHAIN_PASSWORD:-}" "$PREV_SIGN_KEYCHAIN"
    sign+=(--keychain "$PREV_SIGN_KEYCHAIN")
fi
for library in "$app"/Contents/Frameworks/*.dylib; do
    [ -e "$library" ] || continue
    codesign "${sign[@]}" "$library"
done
codesign "${sign[@]}" "$app"
codesign --verify --strict --deep "$app"

# Submits $1 for notarization and staples the ticket to $2.
notarize() {
    local result status submission attempt staple
    result=$(xcrun notarytool submit "$1" "${notary[@]}" --wait --output-format json) || true
    status=$(plutil -extract status raw -o - - <<<"$result" 2>/dev/null || echo none)
    if [ "$status" != Accepted ]; then
        echo "notarization of $1: $status" >&2
        echo "$result" >&2
        submission=$(plutil -extract id raw -o - - <<<"$result" 2>/dev/null || true)
        if [ -n "$submission" ]; then
            xcrun notarytool log "$submission" "${notary[@]}" >&2 || true
        fi
        exit 1
    fi
    # The ticket can take a moment to reach Apple's servers after the
    # submission is accepted, so stapling is tried again before failing.
    for attempt in 1 2 3 4 5; do
        if staple=$(xcrun stapler staple "$2" 2>&1); then
            return
        fi
        if [ "$attempt" = 5 ]; then
            echo "$staple" >&2
            exit 1
        fi
        sleep 30
    done
}

if [ -n "${PREV_NOTARY_KEY:-}" ]; then
    notary=(--key "$PREV_NOTARY_KEY" --key-id "${PREV_NOTARY_KEY_ID:?}"
        --issuer "${PREV_NOTARY_ISSUER:?}")
    # The app on its own first, so its ticket goes with it out of the
    # disk image and it opens without asking Apple's servers.
    archive=$(mktemp -d)/prev.zip
    ditto -c -k --keepParent "$app" "$archive"
    notarize "$archive" "$app"
fi
echo "$app"

if $dmg; then
    stage=$(mktemp -d)
    cp -R "$app" "$stage/"
    ln -s /Applications "$stage/Applications"
    image="$out/$name-$version-$(uname -m).dmg"
    rm -f "$image"
    hdiutil create -volname "$name" -srcfolder "$stage" -ov -format UDZO "$image" >/dev/null
    if [ "$identity" != - ]; then
        image_sign=(--force --sign "$identity" --timestamp)
        if [ -n "${PREV_SIGN_KEYCHAIN:-}" ]; then
            image_sign+=(--keychain "$PREV_SIGN_KEYCHAIN")
        fi
        codesign "${image_sign[@]}" "$image"
    fi
    if [ -n "${PREV_NOTARY_KEY:-}" ]; then
        notarize "$image" "$image"
    fi
    echo "$image"
fi
