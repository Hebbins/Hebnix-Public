# Hebnix

Hebnix is a Rocket League desktop application with an egui interface and Lua plugins.

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
