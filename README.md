# Hebnix

Hebnix is an intelligent wrapper that acts as a gateway to Rocket League's Stats API, Configuration Files, game files and plugins. The idea is to consolidate several Rocket League Quality of Life tools into 1 platform that is simple to work with, user friendly and feature rich.

[Hebnix Website](https://hebnix.com)\
[Hebnix Plugins](https://hebnix.com/plugins)\
[Hebnix Download](https://hebnix.com/download)\
[Hebnix Developer Documentation](https://docs.hebnix.com)

## Build

Install Rust with the MSVC toolchain, then run:

    cd hebnix_rs
    cargo build --release

Build the optional bridge executable with `rlapi_bridge/build.bat`.

Set `$env:HEBNIX_BASE_DIR` to use a different data directory while developing.

## Packaging

    cd hebnix_rs
    ./package.ps1

## Plugins and themes

See the [plugin examples](hebnix_rs/examples/plugins), [theme examples](hebnix_rs/examples/themes), and [documentation](https://hebnix.com/docs).

## Logs

`hebnix.log` and `crash.txt` are written beside the executable. Set `RUST_LOG=debug` for additional logging.

## License

[LICENSE](LICENSE.md)
