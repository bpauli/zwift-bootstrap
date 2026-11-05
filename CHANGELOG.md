# Changelog

All notable changes to zwift-bootstrap are in this file. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- This changelog.
- `launch` selects the graphics profile of the game from a GPU test of about one second before each start, because the GPU table of Zwift gives many current GPUs the basic profile. It writes the selected profile into the four profile files in `data/configs/` and writes one `graphics:` line on stderr. `zwift-bootstrap graphics <profile>` sets a fixed profile, and `zwift-bootstrap graphics auto` uses the GPU test again. `verify` accepts the changed profile files. See `docs/graphics.md`.

## [0.4.1] - 2026-11-04

### Fixed

- `launch` no longer shows the dialog "Could not run Update process. Zwift may not be up to date." (`ERROR ZA1`) at each start. The game checks the name of its parent process. `launch` now starts the game through a small `ZwiftLauncher.exe` stub, which it writes to `.zwift-bootstrap/` in the install directory.

## [0.4.0] - 2026-11-04

### Added

- `install --window` and `update --window` show the download progress in a small window, with a **Cancel** button. The window opens only when there is something to download. Cancel stops the command with exit code 130. Without a display, the command continues in text mode.

### Changed

- The desktop entry shows the progress window on the first start, in place of the "Installing Zwift" notification. A cancel in the window exits without starting the game.
- Building from source needs Rust 1.95.

## [0.3.1] - 2026-11-04

### Fixed

- `launch` no longer passes an empty `--token=` to the game. With an empty value, the game showed "Oops. Wrong email/password" before the user typed anything.
- The desktop entry now installs the game on the first start. Before, it showed "Zwift update failed" on a new system and started nothing. A notification tells the user that the first download starts.

### Changed

- The README tells how to enable video routes with the Vulkan renderer of Wine.
- The build instructions moved to `docs/building.md`.

## [0.3.0] - 2026-11-04

### Added

- Support for Bluetooth LE sensors through BlueZ. `launch` replaces `BleWin10Lib.dll` and `BleWin10Lib_fallback.dll` with its own DLL and starts a helper process that does the Bluetooth work. `verify` accepts the replaced DLLs.

### Fixed

- The Bluetooth LE shim builds with MinGW-w64 before version 10, which has no `afunix.h`.

## [0.2.0] - 2026-11-03

### Added

- Support for ANT+ USB sticks. `launch` writes a `libusb0.dll` next to `ZwiftApp.exe` that sends the ANT+ traffic through the Linux serial device of the stick. `ANT_PORT` selects another device.

### Changed

- Releases build in an Ubuntu 22.04 container. The binary runs on Ubuntu 22.04, Debian 12 and later.

## [0.1.1] - 2026-10-20

### Added

- A `.deb` package for each release, and an APT repository.

## [0.1.0] - 2026-10-20

### Added

- First release. `zwift-bootstrap` installs and updates the Zwift game files from the Zwift CDN, with length and CRC-32 checks, and starts `ZwiftApp.exe` with Wine.
- The `zwift-start` script and a desktop entry.
- Release builds for linux_amd64 and an AUR package.

[Unreleased]: https://github.com/bpauli/zwift-bootstrap/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/bpauli/zwift-bootstrap/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/bpauli/zwift-bootstrap/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/bpauli/zwift-bootstrap/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/bpauli/zwift-bootstrap/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/bpauli/zwift-bootstrap/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/bpauli/zwift-bootstrap/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/bpauli/zwift-bootstrap/releases/tag/v0.1.0
