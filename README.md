# Raw Thumbnailer

A simple command-line tool and D-Bus service to generate thumbnails for raw image files.

## Building

To build the project, you need to have Rust and Cargo installed. You can install them from [rust-lang.org](https://www.rust-lang.org/).

Once you have Rust and Cargo installed, you can build the project by running the following command in the project directory:

```bash
cargo build --release
```

The executable will be located at `target/release/raw-thumbnailer`.

## Usage

### Command-line

To generate a thumbnail from the command-line, you can run the following command:

```bash
./target/release/raw-thumbnailer <input.raw> <output.png>
```

### D-Bus service

The tool can also be run as a D-Bus service. To start the service, run the following command:

```bash
./target/release/raw-thumbnailer --dbus
```

The service will be available at `org.gnome.RawThumbnailer`.
