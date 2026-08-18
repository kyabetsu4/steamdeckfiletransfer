#!/bin/sh
set -eu

bundle_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
binary_source="$bundle_dir/sdft-deck"
launcher_source="$bundle_dir/io.github.kyabetsu4.SteamDeckFileTransfer.desktop.in"
icon_source="$bundle_dir/steam-deck-file-transfer.svg"

install_root="$HOME/Applications/SteamDeckFileTransfer"
applications_dir="$HOME/.local/share/applications"
icons_dir="$HOME/.local/share/icons/hicolor/scalable/apps"

show_error() {
    if command -v kdialog >/dev/null 2>&1; then
        kdialog --error "$1" --title "Steam Deck File Transfer"
    else
        printf '%s\n' "$1" >&2
    fi
}

if [ "$(uname -m)" != "x86_64" ]; then
    show_error "This installer is for x86-64 Steam Deck systems."
    exit 1
fi

if [ ! -f "$binary_source" ]; then
    show_error "The receiver file is missing. Extract the complete download before installing."
    exit 1
fi

if [ ! -f "$launcher_source" ] || [ ! -f "$icon_source" ]; then
    show_error "Installer files are incomplete. Extract the complete download and try again."
    exit 1
fi

mkdir -p "$install_root" "$applications_dir" "$icons_dir"
install -m 0755 "$binary_source" "$install_root/sdft-deck"
install -m 0644 "$icon_source" "$icons_dir/steam-deck-file-transfer.svg"
launcher_built="$install_root/io.github.kyabetsu4.SteamDeckFileTransfer.desktop"
awk -v executable="$install_root/sdft-deck" \
    '{ if ($0 == "Exec=@EXEC@") print "Exec=\"" executable "\""; else print }' \
    "$launcher_source" > "$launcher_built"
chmod 0755 "$launcher_built"
install -m 0755 "$launcher_built" "$applications_dir/io.github.kyabetsu4.SteamDeckFileTransfer.desktop"

if command -v xdg-user-dir >/dev/null 2>&1; then
    desktop_dir=$(xdg-user-dir DESKTOP 2>/dev/null || true)
else
    desktop_dir=""
fi
if [ -z "$desktop_dir" ]; then
    desktop_dir="$HOME/Desktop"
fi
mkdir -p "$desktop_dir"
install -m 0755 "$launcher_built" "$desktop_dir/Steam Deck File Transfer.desktop"

if command -v gio >/dev/null 2>&1; then
    gio set "$desktop_dir/Steam Deck File Transfer.desktop" metadata::trusted true >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$applications_dir" >/dev/null 2>&1 || true
fi

if command -v kdialog >/dev/null 2>&1; then
    if kdialog --yesno \
        "Installed successfully. A Steam Deck File Transfer shortcut is now on your Desktop.\n\nOpen it now?" \
        --title "Steam Deck File Transfer"; then
        "$install_root/sdft-deck" >/dev/null 2>&1 &
    fi
else
    printf 'Installed successfully: %s\n' "$desktop_dir/Steam Deck File Transfer.desktop"
fi
