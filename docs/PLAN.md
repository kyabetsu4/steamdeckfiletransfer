# Product and implementation plan

## Goal

Make sending files to a Steam Deck feel like using a nearby-device share:
open both apps, pair once, drag in files on the PC, and press **Send**.

The first release is deliberately one-way. It does not synchronize folders,
delete remote files, expose the Deck to the internet, or require root access.
Those constraints keep the application small and reduce the chance of data
loss.

## Product boundaries

### Included in v1

- Send one or many files and complete folder trees from Windows to SteamOS.
- Automatic Deck discovery on the same LAN, with manual IP entry as fallback.
- One-time pairing with a short code shown on both screens.
- Remember explicitly paired devices.
- Choose a receive folder; default to `~/Downloads/SteamDeckFileTransfer`.
- Per-file and overall progress, transfer speed, cancellation, and clear errors.
- Resume an interrupted file when its size and partial content still match.
- Verify every completed file before making it visible at its final name.
- Ask before overwriting; default to creating a unique name.
- Run without administrator/root access or changing SteamOS read-only mode.

### Explicitly excluded from v1

- Internet relay, cloud storage, accounts, telemetry, or advertising.
- Deck-to-PC transfer, bidirectional sync, mirroring, and remote deletion.
- Remote file browsing or arbitrary command execution.
- Android, macOS, and general Linux desktop support.
- Automatic updates. Signed, manual releases are simpler for the first version.

## Architecture

```text
Windows PC                                      Steam Deck
+----------------------+                        +----------------------+
| sdft-pc native UI   |                        | sdft-deck native UI |
|                      |  encrypted TCP stream  |                      |
| picker / drop zone   |=======================>| receiver / approvals |
| queue / progress     |   same local network   | safe file writer     |
+----------+-----------+                        +----------+-----------+
           |                                               |
           +--------------- sdft-core ---------------------+
                protocol, identities, paths, hashing
```

Use one Rust workspace and one small native UI toolkit. Sharing the protocol,
validation, and transfer state machine prevents the two apps from drifting.
Avoid Electron, a bundled browser, a database, and a permanent central server.

Recommended UI toolkit: `egui/eframe` with the OpenGL renderer. It is portable,
works with touch and mouse, and keeps the UI in the same language as the transfer
engine. If SteamOS runtime testing exposes graphics compatibility problems, the
fallback is Slint with its software renderer; the network and core crates remain
unchanged.

## Process model

For the first release, the Deck receives only while its app is open. This is
predictable in Desktop Mode and Game Mode and avoids an always-running service.
After v1 is stable, offer an **Start receiver at login** option implemented as a
user-level systemd service. It must be opt-in and use the same binary with a
`--receiver` argument.

## Discovery and connection

1. The PC sends a small UDP discovery probe to the local subnet every two
   seconds while the device screen is open.
2. The Deck answers with protocol version, display name, TCP port, and a random
   per-launch nonce. It never includes filesystem information.
3. The PC opens a TCP connection to the advertised address.
4. Manual `IP:port` entry uses the exact same handshake and is always available
   for networks that block broadcast traffic.

Discovery packets are hints only. They never authorize a transfer.

## Pairing and transport security

- The Deck creates a persistent device identity on first launch.
- The transport is encrypted with TLS using that device identity.
- On first contact, both apps display the same six-digit short authentication
  string derived from the connection transcript and Deck identity.
- The user confirms that the numbers match. The PC then pins the Deck identity,
  and the Deck stores the PC's random 256-bit authorization token.
- Later connections reject a changed Deck identity or missing/revoked PC token.
- Pairing records can be removed from either app.
- The listener binds only to private/local interfaces by default. Documentation
  explicitly warns against router port forwarding.

The code must use established cryptographic libraries. It must not implement
encryption primitives or invent a password-derived cipher.

## Transfer protocol

Use a versioned, length-prefixed control protocol followed by raw file chunks.
Control frames are serialized with a compact, bounded format. Every length,
count, filename, and metadata field has a hard maximum before allocation.

```text
HELLO -> AUTH -> OFFER -> ACCEPT -> FILE_START -> CHUNKS -> FILE_END -> COMPLETE
```

- `OFFER` contains normalized relative paths, sizes, modification times, and the
  total byte/file count.
- `ACCEPT` communicates destination, collision decisions, and resume offsets.
- Data chunks use backpressure; the sender never queues an entire file in RAM.
- BLAKE3 verifies each file. A mismatch keeps the partial file and reports a
  retryable error.
- A heartbeat and idle timeout distinguish a slow copy from a dead connection.
- Protocol version negotiation fails clearly instead of guessing compatibility.

## Filesystem safety

All writes are constrained beneath the user-selected receive directory.

- Reject absolute paths, drive prefixes, `..`, NUL bytes, empty leaf names, and
  platform-invalid names.
- Never follow a symlink or junction while creating destination paths.
- Write to an app-owned partial path, flush it, verify its hash, then atomically
  rename it to the final name.
- Do not preserve executable bits, ownership, ACLs, or Windows alternate data
  streams in v1.
- Check free space before acceptance and again when writes fail.
- Sanitize display text separately from filesystem paths.
- Cancellation leaves resumable partial data, never a truncated final file.

The default collision action is **Keep both**. Overwrite is allowed only after a
visible confirmation and still uses a temporary file plus atomic replacement.

## UI scope

### PC

- Device selector with online/paired state and **Add by IP**.
- Drag-and-drop area plus **Add files** and **Add folder**.
- A compact queue showing name, size, state, speed, and overall progress.
- **Send**, **Cancel**, and **Retry**; no remote file manager.

### Deck

- Large receiver on/off state suitable for touch and controller navigation.
- Pairing confirmation with the six-digit comparison code.
- Receive-folder picker and free-space indicator.
- Incoming transfer approval showing PC name, item count, and total size.
- Current/recent transfer list and a button to open the destination folder.

## Local data

Store only identity, paired-device records, preferences, and resumable-transfer
manifests.

- Windows: `%LOCALAPPDATA%/SteamDeckFileTransfer/`
- SteamOS: `$XDG_DATA_HOME/steamdeck-file-transfer/` with the standard fallback
  under `~/.local/share/`

Logs rotate and omit file contents, authorization tokens, and full private paths
at normal log levels. There is no telemetry.

## Packaging

### Windows

Ship a signed portable `.exe` first, then an optional per-user installer. The
application should not need elevation. Windows Firewall permission is expected
for LAN discovery and outgoing/incoming pairing traffic.

### Steam Deck

Ship a self-contained x86-64 bundle installed entirely in the user's home
directory with a `.desktop` launcher. It must not use `pacman`, write to the OS
image, or require disabling read-only mode. Also provide instructions for adding
the launcher as a non-Steam application for Game Mode. Prepare a Flatpak manifest
after the binary is proven on real hardware; Flatpak is the preferred long-term
distribution format on SteamOS.

## Repository layout

```text
apps/
  pc/                 Windows sender application
  deck/               SteamOS receiver application
crates/
  core/               Shared protocol and filesystem safety
docs/
  PLAN.md
```

Split `core` later only if compile times or ownership justify it. Prematurely
creating many crates adds ceremony without making the binaries smaller.

## Delivery milestones

### M0 - foundation

- Workspace, shared protocol constants, path-safety code, CI skeleton.
- Unit tests for hostile and cross-platform path inputs.

### M1 - transfer engine

- Loopback sender/receiver with offers, bounded streaming, partial files,
  cancellation, hashing, and atomic completion.
- No GUI and no discovery yet; integration tests transfer generated files.

### M2 - usable LAN alpha

- Minimal native PC and Deck screens.
- UDP discovery, manual IP fallback, transfer approvals, collision handling.
- Test PC-to-Deck over Wi-Fi and Ethernet with large files and folder trees.

### M3 - secure beta

- TLS identity, comparison-code pairing, pinning, revocation, and secure local
  credential storage.
- Resume after app restart and network interruption.
- Fuzz/control-frame tests and path traversal/symlink tests.

### M4 - distributable v1

- Controller/touch polish, accessibility labels, rotating diagnostics.
- Windows portable release and Deck home-directory installer/uninstaller.
- Signed checksums, concise setup guide, and repeatable release workflow.

## Acceptance criteria for v1

- Transfer a 20 GB file without loading it into memory or corrupting the result.
- Transfer 10,000 small files while keeping the UI responsive.
- Resume after Wi-Fi loss without rewriting completed files.
- Reject absolute paths, traversal, symlink escapes, malformed frames, unknown
  peers, and changed pinned identities.
- Never place a partial file at the final destination name.
- Stay below a 50 MB compressed download per app and below 100 MB idle memory on
  target hardware; measure release builds rather than assuming.
- Install and uninstall on SteamOS without root and survive an OS update.

## Testing strategy

- Unit tests: frame bounds, path normalization, collision names, state-machine
  transitions, and resume manifests.
- Property/fuzz tests: parsers and path inputs.
- Integration tests: loopback transfer, injected disconnects, disk-full errors,
  cancellation, retries, and hash mismatches.
- Platform tests: Windows Defender/Firewall, SteamOS Desktop Mode, Game Mode,
  Wayland/X11 behavior, controller/touch, suspend/resume, Wi-Fi isolation, and
  mixed Unicode filenames.
- Release checks: binary size, idle memory, dependency/license audit, malware
  scan, clean-machine install, upgrade, and uninstall.

## First implementation slice

Build M1 before spending time on visual polish. A reliable loopback transfer
engine makes every later UI decision cheap; a polished UI over unsafe file writes
does not. The first demo should send a selected directory to a temporary receiver,
interrupt halfway, reconnect, resume, and produce matching hashes.

