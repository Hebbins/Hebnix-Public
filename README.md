<div align="center">

# Hebnix

### An open-source Rocket League toolkit built for the post-EAC era.

Access Rocket League data, build plugins, manage workshop maps, customise game files, and more — all from one platform.

[Website](https://hebnix.com) · [Download](https://hebnix.com/download) · [Plugins](https://hebnix.com/plugins) · [Documentation](https://docs.hebnix.com) · [Discord](https://discord.gg/yr6xXb5wQd)

</div>

---

## 🚀 What is Hebnix?

**Hebnix** is a free and open-source Rocket League toolkit designed to bring useful tools, customisation and community-developed plugins together in one application.

Unlike traditional injected mods, Hebnix operates externally and provides plugins with controlled access to Rocket League data and local resources.

Hebnix acts as a gateway between Rocket League and community plugins, exposing functionality such as:

- 📊 **Rocket League StatsAPI** — Access live game, player and event data
- 🏆 **TRN Data** — Retrieve player ranks and competitive information
- 🧩 **Plugin System** — Build and install community-created Lua plugins
- 🗺️ **Workshop Maps** — Download, manage and launch custom maps
- 🎨 **Game Customisation** — Manage supported local items, decals and game files
- ⚙️ **Configuration Access** — Read and work with local Rocket League configuration
- 🎭 **Themes** — Customise the Hebnix interface
- 🔌 **Developer APIs** — HTTP, crypto, storage, UI and other utilities for plugin developers

The goal is simple: provide a modern platform for Rocket League tools that is **open, extensible and easy to use**.

---

## 🧩 Plugins

Hebnix includes a Lua-based plugin system that allows developers to build tools without needing to modify or inject code into the Rocket League process.

Plugins can interact with Hebnix APIs for functionality including:

```text
Rocket League StatsAPI
TRN Player Data
HTTP Requests
Plugin Storage
Configuration Files
Assets
Cryptography
UI Windows
Drawing & Overlays
```

Browse community plugins:

👉 **[hebnix.com/plugins](https://hebnix.com/plugins)**

Interested in creating your own?

👉 **[Developer Documentation](https://docs.hebnix.com)**

Example projects are also available in:

- [`hebnix_rs/examples/plugins`](hebnix_rs/examples/plugins)
- [`hebnix_rs/examples/themes`](hebnix_rs/examples/themes)

---

## 🛡️ Open Source

Hebnix is completely open source.

This includes the application, plugin system and installer, allowing anyone to inspect how Hebnix works, build it themselves or contribute to the project.

Portable releases are also available for users who prefer not to install Hebnix.

We welcome bug reports, feature suggestions, plugins and contributions from the Rocket League community.

---

## 💻 Building Hebnix

### Requirements

- Windows 10/11
- [Rust](https://www.rust-lang.org/tools/install)
- MSVC toolchain

Clone the repository and build the release version:

```powershell
cd hebnix_rs
cargo build --release
```

The compiled application will be available in:

```text
hebnix_rs/target/release/
```

### RLAPI Bridge

The optional bridge executable can be built using:

```powershell
rlapi_bridge/build.bat
```

### Development Data Directory

By default, Hebnix uses its normal application data directory.

During development, you can override this by setting:

```powershell
$env:HEBNIX_BASE_DIR = "C:\Path\To\Hebnix"
```

---

## 📦 Packaging

To create a distributable build:

```powershell
cd hebnix_rs
./package.ps1
```

---

## 📝 Logs

Hebnix writes diagnostic information beside the executable:

```text
hebnix.log
crash.txt
```

For additional debugging information, enable debug logging:

```powershell
$env:RUST_LOG = "debug"
```

Then launch Hebnix from the same terminal.

---

## 🤝 Contributing

Contributions are welcome.

You can help Hebnix by:

- 🧩 Creating plugins
- 🎭 Creating themes
- 🐛 Reporting bugs
- 💡 Suggesting features
- 🧪 Testing new releases
- 📖 Improving documentation
- 💻 Contributing code

If you're interested in developing plugins or contributing to Hebnix itself, join the community on **[Discord](https://discord.gg/yr6xXb5wQd)**.

---

## 🌐 Community

Have a plugin idea, found a bug, or just want to follow development?

**[Join the Hebnix Discord](https://discord.gg/yr6xXb5wQd)**

You can also:

⭐ Star the repository to support the project  
🧩 Share your plugins with the community  
🐛 Open an issue if you find a problem  
🔀 Submit a pull request if you'd like to contribute

---

## 📜 License

Hebnix is distributed under the terms of the [LICENSE](LICENSE.md).

---

<div align="center">

### Built for the Rocket League community.

**[Download Hebnix](https://hebnix.com/download)** · **[Documentation](https://docs.hebnix.com)** · **[Discord](https://discord.gg/yr6xXb5wQd)**

</div>
