#!/usr/bin/env bash
# Installs a built prev and its data files into a system layout, for
# packages: the binary, desktop entry, icons, AppStream metadata, man page
# and licenses.
#
#   scripts/dist-install.sh BINARY [DESTDIR] [PREFIX] [ID]
#
# BINARY is a release build made with PREV_PRODUCTION=1. DESTDIR is the
# staging root (empty for a direct install) and PREFIX defaults to /usr.
# ID names the desktop entry, icon and metadata, run.prev unless given:
# the Flatpak's is run.prev.Prev, as Flatpak ids take three parts.
set -euo pipefail

binary=${1:?usage: dist-install.sh BINARY [DESTDIR] [PREFIX] [ID]}
destdir=${2:-}
prefix=${3:-/usr}
root=$(cd "$(dirname "$0")/.." && pwd)
id=run.prev
as=${4:-$id}
share="$destdir$prefix/share"

install -Dm755 "$binary" "$destdir$prefix/bin/prev"
install -Dm644 "$root/data/$id.desktop" "$share/applications/$as.desktop"
install -Dm644 "$root/data/$id.metainfo.xml" "$share/metainfo/$as.metainfo.xml"
(cd "$root/data/icons" && find hicolor -type f) | while read -r icon; do
    install -Dm644 "$root/data/icons/$icon" "$share/icons/${icon/\/$id./\/$as.}"
done
if [ "$as" != "$id" ]; then
    # What the desktop entry and metadata name, to match.
    sed -i "s/^Icon=$id\$/Icon=$as/; s/^StartupWMClass=$id\$/StartupWMClass=$as/" \
        "$share/applications/$as.desktop"
    sed -i "s|<id>$id</id>|<id>$as</id>|; s|>$id.desktop</launchable>|>$as.desktop</launchable>|" \
        "$share/metainfo/$as.metainfo.xml"
fi
install -d "$share/man/man1"
gzip -9n <"$root/docs/prev.1" >"$share/man/man1/prev.1.gz"
chmod 644 "$share/man/man1/prev.1.gz"
install -Dm644 "$root/LICENSE" "$share/licenses/prev/LICENSE"
install -Dm644 "$root/docs/THIRD-PARTY.md" "$share/licenses/prev/THIRD-PARTY.md"
for font_license in OFL.txt OFL-DancingScript.txt LICENSE-MaterialSymbols.txt; do
    install -Dm644 "$root/crates/prev/assets/fonts/$font_license" \
        "$share/licenses/prev/fonts/$font_license"
done
install -Dm644 "$root/README.md" "$share/doc/prev/README.md"
