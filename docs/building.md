# Build zwift-bootstrap from source

You need these steps only to build zwift-bootstrap yourself. The AUR package, the APT package and the release files contain the compiled binary. See [Install](../README.md#install).

## Requirements

- Rust 1.95 or later.
- The MinGW-w64 C compiler. It builds the two Windows DLLs that `launch` writes next to `ZwiftApp.exe`: `libusb0.dll` for ANT+ sensors and the replacement `BleWin10Lib.dll` for Bluetooth LE sensors. The binary contains both DLLs. Set `MINGW_CC` to use a different C compiler.
- The D-Bus development files. The Bluetooth LE bridge talks to BlueZ through D-Bus.

On Arch Linux:

```sh
pacman -S rust mingw-w64-gcc dbus
```

On Debian and Ubuntu:

```sh
apt install gcc-mingw-w64-x86-64 libdbus-1-dev pkg-config
```

If the Rust package of your release is older than 1.95, install Rust with [rustup](https://rustup.rs).

## Build and install

```sh
cargo install --path .
```

This puts `zwift-bootstrap` and the Bluetooth LE helper `zwift-bootstrap-ble` in `~/.cargo/bin`. Give the helper its capability, see the README, "Bluetooth LE sensors":

```sh
sudo setcap cap_net_admin+ep ~/.cargo/bin/zwift-bootstrap-ble
```

## Run the tests

CI runs these commands on each push:

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```
