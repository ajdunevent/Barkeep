# Barkeep

Barkeep, the BARcode KEEPer, is an adaptive loyalty card wallet for mobile Linux. Cards are stored on the device, added by scanning or typing, and shown as full-screen barcodes ready for a checkout scanner. It is built with Rust, GTK4 and libadwaita for Phosh, and is developed on a FuriLabs FLX1S running FuriOS.

Barkeep keeps everything local. It has no accounts and makes no network connections of its own.

## Features

- Add a card by scanning with the camera, scanning from an image, or entering it manually.
- Show any card's barcode or QR code full-screen, with copy and share actions for the code.
- Organize cards into groups, search them, star them, and archive them.
- Set validity dates on cards and hide cards that are not currently valid.
- Import from Barkeep exports, Catima, Loyalty Card Keychain, UBcards, FidMe, Voucher Vault and Apple Wallet (`.pkpass`) files. Unrecognized text files are parsed on a best-effort basis after asking for confirmation.
- Export the whole collection as a Barkeep `.json` file.

## Install

### Flatpak

<a href="https://ajdunevent.github.io/AJHub/"><picture><source media="(prefers-color-scheme: dark)" srcset="https://ajdunevent.github.io/AJHub/badges/button-dark.svg"><img src="https://ajdunevent.github.io/AJHub/badges/button-light.svg" alt="AJHub, published with FrogPak" height="48"></picture></a>
(x86_64 and aarch64 builds)

<a href="https://furihub.github.io/repo/"><picture><source media="(prefers-color-scheme: dark)" srcset="https://furihub.github.io/repo/badges/button-dark.svg"><img src="https://furihub.github.io/repo/badges/button-light.svg" alt="FuriHub, published with FrogPak" height="48"></picture></a>
(aarch64 builds)

### Debian package

Build on the target device, since a package built elsewhere links against the wrong libraries:

```
cargo build --release
cargo deb -p barkeep
sudo apt install ./target/debian/barkeep_<version>_arm64.deb
```

## Building

Barkeep needs GTK 4.12 or newer, libadwaita 1.5 or newer, GStreamer with its base and good plugins, and the `gtk4paintablesink` GStreamer element for the camera preview.

```
cargo run -p barkeep
```

To build and install a Flatpak with the Flatpak SDK, run `build-aux/dev.sh`. It asks for the architecture, the output (a `.flatpak` file in `dist/`, an install, or both) and the Cargo profile. Building for the other architecture needs QEMU user emulation.

## Notes for the FLX1S

- GTK's default Vulkan renderer causes severe lag on this device. Start the app with `GSK_RENDERER=ngl`. For the Flatpak: `flatpak override --user --env=GSK_RENDERER=ngl net.dunevent.Barkeep`.
- The camera source is chosen at startup from `droidcamsrc`, `libcamerasrc`, `pipewiresrc` and `v4l2src`, in that order. On the FLX1S, `droidcamsrc` is the one that performs well.

## Data

Cards are saved in `$XDG_DATA_HOME/barkeep/cards.json` and interface state in `$XDG_STATE_HOME/barkeep/ui-state.json`. The Flatpak keeps these under `~/.var/app/net.dunevent.Barkeep/`, separate from a Debian package install. To move cards between the two, export from one and import into the other.

## License

GPL-3.0-or-later.
