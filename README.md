# zwift-launcher

A native Linux replacement for the Zwift Windows launcher. The binary `zwift-bootstrap` installs and updates the Zwift game files directly from the Zwift CDN. Then it starts `ZwiftApp.exe` with Wine. You do not need the Windows launcher, .NET or WebView2 in your Wine prefix.

This is an unofficial project. It is not made, supported or approved by Zwift, Inc. "Zwift" is a trademark of Zwift, Inc. Zwift can change its update format at any time, and then this tool stops working until it is updated. You need your own Zwift account.

## What it does

- **Install and update.** Reads the current game version from the Zwift CDN, downloads only the files that changed, and checks the length and CRC-32 of every file. The install directory changes only after all files have passed. An interrupted run continues on the next run.
- **Start the game.** Runs `ZwiftApp.exe` through Wine from the install directory. The game shows its own login screen.
- **Check the files.** `verify` compares every installed file with the manifest.

## Requirements

- Linux on x86_64.
- Rust 1.89 or later, to build.
- Wine, to play. wine-staging 11 works.

## Build and install

```sh
cargo install --path .
```

This puts `zwift-bootstrap` in `~/.cargo/bin`.

## Usage

```sh
zwift-bootstrap check      # local and server version, size of the next update
zwift-bootstrap install    # first install, about 4.8 GiB
zwift-bootstrap update     # bring an install to the server version
zwift-bootstrap verify     # check all files, exit 1 on a problem
zwift-bootstrap launch     # start the game with Wine
```

Options:

- `--dir DIR` sets the install directory. The default is `$XDG_DATA_HOME/zwift` (`~/.local/share/zwift`).
- `--jobs N` sets the number of parallel downloads for `install` and `update`. The default is 8.
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

`contrib/zwift` updates the game and then starts it. `contrib/zwift.desktop` adds Zwift to your application menu.

```sh
install -m755 contrib/zwift ~/.local/bin/zwift
install -m644 contrib/zwift.desktop ~/.local/share/applications/zwift.desktop
```

The desktop entry uses a generic icon. You can set `Icon=` to an image of your choice.

## Graphics tips

- **Laptop battery saver.** On battery, Zwift limits the frame rate to about 20 FPS. Switch off the battery saver in the Zwift settings, or connect the charger.
- **Unknown GPU.** When Zwift does not know your GPU, it uses its basic profile at 1024x576. To force better settings, copy one of the profiles over the basic profile in the install directory:

  ```sh
  cd ~/.local/share/zwift/data/configs
  cp basic.txt basic.txt.orig
  cp ultra.txt basic.txt    # or high.txt or medium.txt
  ```

  After this change, `verify` reports `basic.txt` as changed. A game update can restore the original file.

## Not tested

- Bluetooth and ANT+ sensors under Wine.
- Frame rate during a ride on hardware other than an AMD Radeon 880M.

## How it works

See [docs/protocol.md](docs/protocol.md) for the CDN files and the checksum format.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
