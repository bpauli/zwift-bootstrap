# zwift-bootstrap

A native Linux replacement for the Zwift Windows launcher. The binary `zwift-bootstrap` installs and updates the Zwift game files directly from the Zwift CDN. Then it starts `ZwiftApp.exe` with Wine. You do not need the Windows launcher, .NET or WebView2 in your Wine prefix.

![Zwift on the Champs-Élysées, started with zwift-bootstrap on Linux, next to a browser and a terminal](docs/screenshot.png)

This is an unofficial project. It is not made, supported or approved by Zwift, Inc. "Zwift" is a trademark of Zwift, Inc. Zwift can change its update format at any time, and then this tool stops working until it is updated. You need your own Zwift account.

## What it does

- **Install and update.** Reads the current game version from the Zwift CDN, downloads only the files that changed, and checks the length and CRC-32 of every file. The install directory changes only after all files have passed. An interrupted run continues on the next run.
- **Start the game.** Runs `ZwiftApp.exe` through Wine from the install directory. The game shows its own login screen.
- **Check the files.** `verify` compares every installed file with the manifest.

## Why not Lutris, Bottles or Docker?

- **No Windows launcher.** The other ways install the Zwift Windows launcher in Wine. The launcher needs .NET and WebView2 in the Wine prefix, and these installs often fail. zwift-bootstrap gets the game files directly from the Zwift CDN.
- **Bluetooth LE sensors in the game.** Wine cannot run the Bluetooth LE library of the game. zwift-bootstrap connects your trainer and sensors through BlueZ, from the pairing screen of the game. You do not need the Zwift Companion app as a bridge.
- **ANT+ USB sticks.** Wine has no driver for ANT sticks. zwift-bootstrap sends the ANT+ traffic through the Linux serial device of the stick.
- **No container.** Wine runs on your system, with your GPU driver and tools such as `gamemoderun` and MangoHud.

## Requirements

- Linux on x86_64.
- Wine. wine-staging 11 works.
- BlueZ, for Bluetooth LE sensors.

## Install

On Arch Linux, install the package from the AUR:

```sh
yay -S zwift-bootstrap-bin
```

The package also installs `zwift-start` and the desktop entry.

On Debian 12 or later and Ubuntu 22.04 or later, add the APT repository and install the package:

```sh
curl -fsSL https://bpauli.de/apt-packages/key.gpg | sudo tee /usr/share/keyrings/bpauli-apt.gpg > /dev/null
echo "deb [signed-by=/usr/share/keyrings/bpauli-apt.gpg] https://bpauli.de/apt-packages stable main" | sudo tee /etc/apt/sources.list.d/bpauli.list
sudo apt update
sudo apt install zwift-bootstrap
```

On Fedora 41 or later and AlmaLinux 10, download the `.rpm` file from the [GitHub release page](https://github.com/bpauli/zwift-bootstrap/releases/latest) and install it:

```sh
sudo dnf install ./zwift-bootstrap-<version>-1.x86_64.rpm
```

The package also installs `zwift-start` and the desktop entry, and gives `zwift-bootstrap-ble` its capability. Red Hat Enterprise Linux 9 and its rebuilds, for example Rocky Linux 9, cannot install the package. They have glibc 2.34, and zwift-bootstrap needs glibc 2.35 or later.

Each release also has the `.deb` file, the `.rpm` file and the binaries as a `.tar.gz` file on its GitHub release page. Put `zwift-bootstrap` and the Bluetooth LE helper `zwift-bootstrap-ble` into the same directory, and see "Bluetooth LE sensors" for the capability of the helper. The binaries need the D-Bus library (`libdbus-1`), which most desktops already have.

To build from source, see [docs/building.md](docs/building.md).

## Usage

```sh
zwift-bootstrap check      # local and server version, size of the next update
zwift-bootstrap install    # first install, about 4.8 GiB
zwift-bootstrap update     # bring an install to the server version
zwift-bootstrap verify     # check all files, exit 1 on a problem
zwift-bootstrap launch     # start the game with Wine
zwift-bootstrap graphics   # show or set the graphics profile
```

Options:

- `--dir DIR` sets the install directory. The default is `$XDG_DATA_HOME/zwift` (`~/.local/share/zwift`).
- `--jobs N` sets the number of parallel downloads for `install` and `update`. The default is 8.
- `--window` shows the progress of `install` and `update` in a small window. The window opens only when there is something to download, and it has a **Cancel** button. A cancel stops the command with exit code 130. Without a display, the command shows the progress on stderr only.
- `WINE` sets the Wine command for `launch`. The default is `wine`.
- `WINEPREFIX` sets the Wine prefix for `launch`. The default is `$XDG_DATA_HOME/zwift-wine`.
- Arguments after `--` go to the game, for example `zwift-bootstrap launch -- --some-flag`.

Downloads are staged in `$XDG_CACHE_HOME/zwift/downloads` (`~/.cache/zwift/downloads`).

## Set up Wine

1. Install Wine. On Arch Linux: `pacman -S wine-staging`.
2. Create the prefix and set Windows 10:

   ```sh
   export WINEPREFIX=~/.local/share/zwift-wine
   wineboot -i
   wine winecfg -v win10
   ```

3. Start the game with `zwift-bootstrap launch`.

The game writes its logs and settings to `Documents/Zwift` in the prefix. Wine usually links that folder to `~/Documents`, so look in `~/Documents/Zwift`.

## Desktop entry

`contrib/zwift-start` installs or updates the game and then starts it. `contrib/zwift.desktop` adds Zwift to your application menu.

The first start downloads about 4.8 GiB before the game opens. During the download, a small window shows the progress, the speed and the time that is left. When you click **Cancel**, the download stops and the game does not start. The next start continues the download. A start without an update shows no window. If the update fails, a desktop notification tells you, and the installed version starts.

```sh
install -m755 contrib/zwift-start ~/.local/bin/zwift-start
install -m644 contrib/zwift.desktop ~/.local/share/applications/zwift.desktop
install -Dm644 contrib/zwift.png ~/.local/share/icons/hicolor/192x192/apps/zwift-bootstrap.png
```

The desktop entry uses the Zwift icon from zwift.com.

## Graphics profile

Zwift selects its graphics profile (basic, medium, high or ultra) from a table of GPU names. The table does not know many current GPUs, and an unknown GPU gets basic. So before each start, `launch` runs a GPU test of about one second and selects the profile from the result. The result changes with the power state of a laptop, for example on battery.

`launch` writes the result on stderr:

```text
graphics: ultra (GPU test 6.87 ms on AMD Radeon 880M Graphics (radeonsi, ...))
```

To always use one profile and skip the test:

```sh
zwift-bootstrap graphics high    # or basic, medium, ultra
```

To use the GPU test again, run `zwift-bootstrap graphics auto`. `zwift-bootstrap graphics` without a value shows the setting.

`launch` writes the selected profile into all four profile files in `data/configs/`, and keeps copies of the original files in `.zwift-bootstrap/profiles/`. `verify` accepts the changed profile files. If the GPU test fails, the game selects the profile. The limits of the test come from one laptop. Please send a [hardware report](https://github.com/bpauli/zwift-bootstrap/issues/new?template=hardware-report.yml) with the GPU test value and the profile that works well on your computer. See [docs/graphics.md](docs/graphics.md) for the details.

## Graphics tips

- **Laptop battery saver.** On battery, Zwift limits the frame rate to about 20 FPS. Switch off the battery saver in the Zwift settings, or connect the charger.

## Video routes

With the default Wine settings, Zwift shows a technical error when you select **Video** in the route list. Zwift needs H.264 video decoding through Direct3D 11, and Wine supplies this only with its Vulkan renderer.

1. Install a 64-bit Vulkan driver for your GPU and the GStreamer H.264 plugins. On Arch Linux: `pacman -S gst-plugins-bad gst-libav` and `vulkan-radeon`, `vulkan-intel` or `nvidia-utils`. On Debian and Ubuntu: `apt install gstreamer1.0-plugins-bad gstreamer1.0-libav mesa-vulkan-drivers`.
2. Set the Vulkan renderer for the game only:

   ```sh
   WINEPREFIX=~/.local/share/zwift-wine wine reg add 'HKCU\Software\Wine\AppDefaults\ZwiftApp.exe\Direct3D' /v renderer /t REG_SZ /d vulkan /f
   ```

3. In the Zwift settings, switch off **Video Screenshots**. With the Vulkan renderer or the GStreamer plugins, the video capture of the game can stop the game or use all of the RAM.
4. Start the game. In `~/Documents/Zwift/Logs/Log.txt`, find the line that starts with `VideoDecoderContext`. `hardware decode supported` tells you that the check passed. `0x887A0004` tells you that Wine does not use Vulkan video decoding. Then make sure that you did step 2 and that your driver supports Vulkan video. On Intel GPUs, you possibly must also set `ANV_VIDEO_DECODE=1`.

To use the default renderer again:

```sh
WINEPREFIX=~/.local/share/zwift-wine wine reg delete 'HKCU\Software\Wine\AppDefaults\ZwiftApp.exe\Direct3D' /f
```

## ANT+ sensors

Wine has no driver for ANT USB sticks. So `launch` writes its own `libusb0.dll` next to `ZwiftApp.exe`. This file sends the ANT+ traffic of the game through the Linux serial device of the stick. `verify` ignores this file, and game updates do not remove it.

1. Connect the stick. Linux shows it as a serial device, for example `/dev/serial/by-id/usb-Dynastream_Innovations_ANT_USBStick2_123-if00-port0`.
2. Make sure that your user can open the device. Most desktops give access to the logged-in user. If not, add your user to the group of the device: `uucp` on Arch Linux, `dialout` on Debian and Ubuntu.
3. Start the game with `zwift-bootstrap launch` and pair the sensors in the pairing screen.

The DLL uses the first device that matches `/dev/serial/by-id/usb-Dynastream_Innovations_*`. To use a different device, set `ANT_PORT`, for example `ANT_PORT=/dev/ttyUSB1 zwift-bootstrap launch`.

Tested with a Dynastream ANT USBStick2 and a Garmin heart rate strap.

## Bluetooth LE sensors

Wine cannot run the Bluetooth LE library of the game (`BleWin10Lib.dll`). So `launch` replaces `BleWin10Lib.dll` and `BleWin10Lib_fallback.dll` in the install directory with its own DLL, and starts a helper process (`zwift-bootstrap-ble`, installed next to `zwift-bootstrap`). The DLL sends the Bluetooth LE requests of the game to the helper, and the helper does the Bluetooth work through BlueZ. The helper stops when the game stops. `verify` accepts the replaced files. A game update can restore the original files, and the next `launch` replaces them again.

1. Make sure that BlueZ runs and that the Bluetooth adapter is on: `bluetoothctl show` must show `Powered: yes`.
2. Start the game with `zwift-bootstrap launch` and pair the sensors in the pairing screen. You do not have to pair the sensors in Linux first.

The helper listens on `$XDG_RUNTIME_DIR/zwift-bootstrap-ble.sock`. If `XDG_RUNTIME_DIR` is not set, or `zwift-bootstrap-ble` is not next to `zwift-bootstrap`, the game starts without Bluetooth LE, and `launch` writes one warning line.

### The CAP_NET_ADMIN capability

The `.deb` package and the `.rpm` package give `/usr/bin/zwift-bootstrap-ble` the file capability `cap_net_admin+ep`. With it, the helper asks the kernel for a fast connection interval (30 ms to 50 ms) before each connection to a sensor. Without it, the kernel uses the interval that the sensor prefers, and the service discovery after the connection is slow: a Wahoo KICKR (180 ms) connects in about 25 s and a Wahoo cadence sensor (990 ms) in about 60 s, in place of about 4 s. BlueZ has no D-Bus API for the connection interval, so the helper sends the request on the Bluetooth management socket of the kernel, which accepts it only from a process with `CAP_NET_ADMIN`.

`CAP_NET_ADMIN` lets a process change the network configuration of the system, for example the interfaces, the routes and the Bluetooth controllers. To keep its use small, the helper drops the capability as soon as it has opened the kernel socket. The helper is a separate file because a binary with a file capability runs in secure mode, where glibc removes `LD_PRELOAD`, `LD_LIBRARY_PATH` and `TMPDIR` from the environment, and `zwift-bootstrap launch` must pass these on to Wine for tools such as `gamemoderun` and MangoHud. The game runs without the capability.

If you use the tarball or a source build, set the capability yourself:

```sh
sudo setcap cap_net_admin+ep "$(command -v zwift-bootstrap-ble)"
```

Without the capability, the helper writes `no CAP_NET_ADMIN` into the log of the game once, and the sensors connect at their preferred interval.

Tested with a Wahoo KICKR (power and smart trainer control), a Wahoo cadence sensor and a Garmin HRM-Pro Plus.

## Not tested

If you use one of these, please send a [hardware report](https://github.com/bpauli/zwift-bootstrap/issues/new?template=hardware-report.yml), also when everything works.

- Bluetooth LE sensors and trainers other than the ones above.
- ANT+ sticks other than the ANT USBStick2.
- Riding a video route. Only the decoding check after the login is tested, on an AMD Radeon 880M with the Vulkan renderer.
- Frame rate during a ride on hardware other than an AMD Radeon 880M. The limits of the GPU test come from this GPU only.

## How it works

See [docs/protocol.md](docs/protocol.md) for the CDN files and the checksum format.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
