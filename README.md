# Raw Thumbnailer

A simple command-line tool and D-Bus service to generate thumbnails for raw image files.

## Installation

### Dependencies

You need to have Rust and Cargo installed to build this project.

### Building and Installing

The easiest way to install `raw-thumbnailer` is using the provided Makefile. This will install the binary to `/usr/local/bin` and the GNOME thumbnailer configuration to `/usr/local/share/thumbnailers`.

```bash
sudo make install
```

### Upgrading

To upgrade from a previous version, it is recommended to uninstall first to ensure a clean state:

```bash
sudo make uninstall
sudo make install
```

After installing or upgrading, you may need to clear the thumbnail cache and restart Nautilus for changes to take effect:

```bash
rm -rf ~/.cache/thumbnails/*
nautilus -q
```

To uninstall:

```bash
sudo make uninstall
```

## Usage

### Command-line

To generate a thumbnail from the command-line:

```bash
raw-thumbnailer [-s <size>] <input.raw> <output.png>
```

- `-s <size>`: Optional. Specify the size of the thumbnail (default is 512). The thumbnail will be scaled to fit within a box of `size x size` while preserving aspect ratio.

### D-Bus service

The tool can also be run as a D-Bus service, which is used by some applications to request thumbnails.

```bash
raw-thumbnailer --dbus
```

The service will be available at `org.gnome.RawThumbnailer`.

## Logs

Logs are written to standard error (stderr). When running as a system service or within GNOME, these logs can typically be viewed using `journalctl`.
