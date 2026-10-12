# softty

A minimal, fast Wayland terminal emulator written in Rust.

## Features

- Native Wayland client, no X11 or XWayland
- Fast startup (10-15ms) and low memory usage
- Software rendering with shared-memory buffers, no GPU or OpenGL required
- Unicode-aware, including correct width for wide characters
- Full keyboard layout support through xkbcommon
- Configurable through a TOML config file

## Benchmark

Startup time and memory usage, compared against [foot](https://codeberg.org/dnkl/foot):

```
Benchmark 1: foot true (40 runs)
                   mean ±     σ    min …   max
  Wall Time [ms]   79.0 ±   7.0   74.3 … 108.6
  Memory [MiB]     31.6 ±   0.1   31.4 …  31.8

Benchmark 2: softty -e true (183 runs)
                   mean ±     σ    min …   max
  Wall Time [ms]   13.1 ±   1.1   11.9 …  17.1   -83.4%  (6.0x faster)
  Memory [MiB]      9.3 ±   0.1    9.0 …   9.4   -70.7%  (3.4x less)
```

## Installation

### Arch Linux
```sh
yay -S softty-git
```
### Other
Softty is not available on other repositories yet, please build it from source.

## Building

Requires Rust 1.70 or newer.

```sh
git clone https://github.com/socatlolmeow/softty
cd softty
cargo build --release
```

The binary ends up in `target/release/softty`.

## Usage

```sh
softty
softty -e <command>
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[GPL-3.0](LICENSE)
