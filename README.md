# Steam Deck File Transfer

A small, local-only file transfer app for sending files and folders from a
Windows PC to a Steam Deck.

The project is intentionally split into two lightweight native applications:

- `sdft-pc`: choose files, find a paired Deck, and send.
- `sdft-deck`: approve pairing, choose the destination, and receive.

Both applications share the same Rust protocol and safety code. There is no
cloud service, account, database, browser runtime, or Electron dependency.

## Status

The LAN alpha is functional. Both apps have small native GUIs and retain a CLI
mode for diagnostics. The transfer engine streams bounded chunks, verifies files
with BLAKE3, resumes partial data, keeps existing destination files by default,
and atomically exposes completed files.

Pairing, transport encryption, and automatic Deck discovery are still pending.
Until those land, use this build only on a trusted private LAN. See
[docs/PLAN.md](docs/PLAN.md) for the full release plan.

## Initial targets

- PC: Windows 10/11, x86-64
- Deck: current SteamOS, x86-64
- Network: same trusted local network
- Direction in v1: PC to Deck

## Run the alpha from source

Requirements: a current Rust toolchain and both devices on the same private LAN.

On the Steam Deck, build and launch the receiver from Desktop Mode:

```sh
cargo run --release --bin sdft-deck
```

Choose the receive folder and press **Start receiver**. Find the Deck's LAN IP
in SteamOS network settings.

On the Windows PC:

```powershell
cargo run --release --bin sdft-pc
```

Enter the Deck IP, drag files or folders onto the window, and press **Send**.

Launching either binary without arguments opens its GUI. CLI equivalents are:

```text
sdft-deck --listen 0.0.0.0:49321 --output /home/deck/Downloads/SteamDeckFileTransfer
sdft-pc --host 192.168.1.42:49321 FILE-OR-FOLDER...
```

The first receiver launch may require allowing TCP port `49321` through the
local firewall. Do not forward this port on a router.

## Install on a Steam Deck without a terminal

Release builds produce `SteamDeckFileTransfer-Deck-x86_64.tar.gz`. On the Deck:

1. Open the download in Dolphin and extract the `SteamDeckFileTransfer` folder.
2. Open that folder.
3. Double-click **Install Steam Deck File Transfer** and choose **Execute**.

The installer places a normal **Steam Deck File Transfer** shortcut on the
Desktop and adds the app to the application menu. It installs entirely under the
current user's home folder and does not ask for a password or modify SteamOS.

The extracted folder also contains a clickable uninstaller. Keep the folder if
you want that convenience; otherwise it is safe to delete after installation.
