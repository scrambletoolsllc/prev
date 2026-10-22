#!/bin/sh
# Installs one of prev's Linux packages on the clean system it runs on, a
# container of the distribution the package is for, and checks that prev
# starts, that its files are in place and that it uninstalls. Run as root
# by .github/workflows/install-test.yml:
#
#   scripts/install-test.sh deb|rpm|appimage|tarball|flatpak DIR
#
# The .deb and .rpm are installed with what they depend on and nothing
# they only recommend, as on a minimal system; the AppImage and tarball
# expect the system's font library (packaging/appimage/build.sh).
set -eu
kind=$1
dir=$2
arch=$(uname -m)
id=run.prev
# Flatpak ids take three parts.
flatpak_id=run.prev.Prev

one() {
    # The one file in DIR matching a pattern.
    set -- $dir/$1
    [ -e "$1" ] || { echo "No $kind package matching $1" >&2; exit 1; }
    echo "$1"
}

starts() {
    version=$("$@" --version)
    case $version in
        "prev "[0-9]*) echo "prev starts: $version" ;;
        *) echo "prev did not start: $version" >&2; exit 1 ;;
    esac
}

# prev loads the Wayland, keyboard and Vulkan libraries as it opens a
# window, so --version does not; the package's dependencies must bring them.
loads() {
    for lib in libwayland-client.so.0 libxkbcommon.so.0 libvulkan.so.1; do
        ldconfig -p | grep -q "$lib" || { echo "$lib is missing after installing" >&2; exit 1; }
    done
}

linked() {
    if ldd "$1" | grep 'not found'; then
        echo "$1 links libraries this system lacks" >&2
        exit 1
    fi
}

installed() {
    test -x /usr/bin/prev
    test -f /usr/share/applications/$id.desktop
    test -f /usr/share/icons/hicolor/scalable/apps/$id.svg
    test -f /usr/share/metainfo/$id.metainfo.xml
}

case $kind in
deb)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends "$(one "prev_*_$(dpkg --print-architecture).deb")"
    installed
    linked /usr/bin/prev
    loads
    starts prev
    apt-get remove -y -qq prev
    test ! -e /usr/bin/prev
    ;;
rpm)
    dnf install -y -q --setopt=install_weak_deps=False "$(one "prev-*.$arch.rpm")"
    installed
    linked /usr/bin/prev
    loads
    starts prev
    dnf remove -y -q prev
    test ! -e /usr/bin/prev
    ;;
appimage)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends libfontconfig1
    # Out of the read-only folder it came in, to be made executable.
    image=/tmp/$(basename "$(one "prev-*-$arch.AppImage")")
    cp "$(one "prev-*-$arch.AppImage")" "$image"
    chmod +x "$image"
    # No FUSE in a container: the AppImage unpacks itself instead.
    export APPIMAGE_EXTRACT_AND_RUN=1
    starts "$image"
    ;;
tarball)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends libfontconfig1
    tar -C / -xzf "$(one "prev-*-$arch-linux.tar.gz")"
    installed
    linked /usr/bin/prev
    starts prev
    ;;
flatpak)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends flatpak ca-certificates dbus
    # flatpak talks to the system and session buses, and runs apps in a
    # runtime folder, which a desktop has and a container does not.
    mkdir -p /run/dbus
    dbus-daemon --system --fork
    export XDG_RUNTIME_DIR=/tmp/runtime
    mkdir -p -m 700 "$XDG_RUNTIME_DIR"
    # For the user, as the README says to install it; the bundle's runtime
    # comes from Flathub.
    dbus-run-session -- sh -eu -c '
        flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
        flatpak install --user -y --noninteractive "$1"
        version=$(flatpak run '$flatpak_id' --version)
        case $version in
            "prev "[0-9]*) echo "prev starts: $version" ;;
            *) echo "prev did not start: $version" >&2; exit 1 ;;
        esac
        flatpak uninstall --user -y --noninteractive '$flatpak_id'
    ' sh "$(one "prev-$arch.flatpak")"
    ;;
*)
    echo "No install test for $kind" >&2
    exit 2
    ;;
esac
echo "The $kind package installs and starts on $(. /etc/os-release && echo "$PRETTY_NAME") ($arch)"
