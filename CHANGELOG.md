# Changelog

All notable changes to zwift-bootstrap are in this file. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- A "Hardware report" issue template for the GPU, the sensors and the ANT+ stick that work on a computer. The README and `docs/graphics.md` link to it.
- The README tells why zwift-bootstrap is different from Lutris, Bottles and Docker.
- Each release has an `.rpm` package for Fedora 41 or later and AlmaLinux 10 next to the `.deb` package, also in the new DNF repository https://bpauli.de/rpm-packages. It contains the same files as the `.deb` package. At install, rpm gives `zwift-bootstrap-ble` the file capability `cap_net_admin=ep`. Red Hat Enterprise Linux 9 and its rebuilds cannot install it, because their glibc 2.34 is too old.

### Changed

- The release binaries are stripped. Before, the two binaries in the tarball contained the symbol table and were together about 6 MB larger. The `.deb` package already contained stripped binaries.

## [0.5.1] - 2026-11-05

### Changed

- The desktop entry shows the Zwift icon from zwift.com in place of a generic game icon. The `.deb` package and the tarball include the icon as `zwift.png`.

## [0.5.0] - 2026-11-05

### Added

- This changelog.
- `launch` selects the graphics profile of the game from a GPU test of about one second before each start, because the GPU table of Zwift gives many current GPUs the basic profile. It writes the selected profile into the four profile files in `data/configs/` and writes one `graphics:` line on stderr. `zwift-bootstrap graphics <profile>` sets a fixed profile, and `zwift-bootstrap graphics auto` uses the GPU test again. `verify` accepts the changed profile files. See `docs/graphics.md`.

### Changed

- Bluetooth LE sensors connect faster. The helper asks the kernel for a connection interval of 30 ms to 50 ms before each connection, in place of the interval that the sensor prefers. The helper also pauses its scan while a sensor connects, because the game scans during the whole pairing screen and a running scan makes a connection about 5 times slower. In the game, a Wahoo KICKR connected in 5 s in place of about 25 s, and a Wahoo cadence sensor in 2 s in place of about 60 s. The helper needs the file capability `cap_net_admin+ep` for this. The `.deb` package sets it at install and depends on `libcap2-bin`. For the tarball and a source build, see the README, "Bluetooth LE sensors". Without the capability, the helper writes `no CAP_NET_ADMIN` into the log of the game once and connects as before.
- The Bluetooth LE helper is its own binary, `zwift-bootstrap-ble`. The package and the tarball install it next to `zwift-bootstrap`, and `launch` starts it from there. The capability is on this file only, so that the environment of `launch` reaches Wine unchanged.

### Fixed

- The Bluetooth LE helper no longer reports a sensor as connected after the game unpaired it while the connection was in progress. The game showed such a sensor with "No Signal".
- The Bluetooth LE helper disconnects all sensors before it ends. Before, a sensor sometimes stayed connected after the game closed. A connected sensor does not advertise, so the next game session did not find it.

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

[Unreleased]: https://github.com/bpauli/zwift-bootstrap/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/bpauli/zwift-bootstrap/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/bpauli/zwift-bootstrap/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/bpauli/zwift-bootstrap/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/bpauli/zwift-bootstrap/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/bpauli/zwift-bootstrap/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/bpauli/zwift-bootstrap/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/bpauli/zwift-bootstrap/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/bpauli/zwift-bootstrap/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/bpauli/zwift-bootstrap/releases/tag/v0.1.0
