#!/bin/sh
set -eu

install_root="$HOME/Applications/SteamDeckFileTransfer"
applications_launcher="$HOME/.local/share/applications/io.github.kyabetsu4.SteamDeckFileTransfer.desktop"
icon="$HOME/.local/share/icons/hicolor/scalable/apps/steam-deck-file-transfer.svg"

if command -v xdg-user-dir >/dev/null 2>&1; then
    desktop_dir=$(xdg-user-dir DESKTOP 2>/dev/null || true)
else
    desktop_dir=""
fi
if [ -z "$desktop_dir" ]; then
    desktop_dir="$HOME/Desktop"
fi

rm -f -- "$install_root/sdft-deck" "$install_root/io.github.kyabetsu4.SteamDeckFileTransfer.desktop"
rmdir -- "$install_root" 2>/dev/null || true
rm -f -- "$applications_launcher" "$icon" "$desktop_dir/Steam Deck File Transfer.desktop"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$HOME/.local/share/applications" >/dev/null 2>&1 || true
fi

if command -v kdialog >/dev/null 2>&1; then
    kdialog --msgbox "Steam Deck File Transfer was removed." --title "Steam Deck File Transfer"
else
    printf 'Steam Deck File Transfer was removed.\n'
fi
