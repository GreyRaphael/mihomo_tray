# mihomo-tray

English | [中文](README_zh.md)

A lightweight, **100% portable** Windows 10/11 system tray controller for the [mihomo](https://github.com/MetaCubeX/mihomo) core, built on Rust 1.100 (`edition = "2024"`) and modern `crates.io` libraries with clean platform boundaries for future Linux/macOS support.

## Highlights

- **Fully Portable**: All configs, mihomo geodata/cache (`-d`), logs, and optional WebUI files stay inside the executable directory (`portable_root`). Relative paths in `tray.yml` resolve relative to `portable_root`.
- **Stop-and-Replace Core**: Click `Quit` to stop the core and exit, drop any updated `mihomo*.exe` into `./core/` (auto-discovered even without renaming if it is the only `mihomo*.exe`), and relaunch (previous `System Proxy`, mode, and `TUN Mode` states are restored automatically via `./config/.tray-state.json`).
- **Stop-and-Replace WebUI**: Drop any static dashboard (`zashboard`, `metacubexd`, `yacd-meta`) into `./ui/` (passed automatically as `-ext-ui`), or configure `ui.web_url` in `tray.yml`. Opens in your default browser.
- **No Service Required**: Normal execution runs non-elevated; TUN mode triggers UAC only when needed via a hidden self-helper (`--elevate-start` / `--elevate-stop`).
- **One-Step Subscription Setup**: Click `Set Subscription URL` in the tray menu to configure your subscription in a DPI-aware centered dialog (auto-updates every 12 hours, wired into `Proxy` / `Auto` / `Fallback` groups and refreshed immediately).
- **Persistent State Across Restarts**: Remembers `System Proxy`, routing mode (`Rule` / `Global` / `Direct`), and `TUN Mode` in `./config/.tray-state.json` across `Quit` and `More -> Restart Application` while still cleanly disabling OS proxy on shutdown.
- **Ready-to-Use Release Bundle**: GitHub Releases bundle `mihomo-tray.exe` (with embedded icon), the latest `mihomo-windows-amd64-v3` core, `zashboard` (`dist-no-fonts`), default `config/config.yaml`, and Loyalsoldier `geoip.dat` / `geosite.dat` / `Country.mmdb`.
- **Two-Phase Menu + Procedural Icon**: Streamlined menu (`Open WebUI`, `Set Subscription URL`, modes, `System Proxy`, `TUN Mode`, `Proxy Groups`, `Open`, `More`, `Quit`), menu-open guards so hovering never dismisses the menu, and a 32x32 procedural RGBA dual-axis icon (`outer ring = state`, `center dot = mode`).

## Portable Layout

```text
mihomo-tray/
├── mihomo-tray.exe
├── tray.yml               # Auto-generated from assets/tray.sample.yml on first run
├── core/
│   └── mihomo.exe         # Or any single mihomo-*.exe
├── config/
│   ├── config.yaml        # mihomo configuration (-d defaults to ./config)
│   └── .tray-state.json   # Last-used System Proxy / mode / TUN state
├── ui/                    # Optional static WebUI files (index.html, ...)
└── logs/
    └── mihomo-tray.log
```

## Build & Check

```powershell
# Windows 10/11 release build
cargo build --release

# Unit tests (cross-platform core & logic)
cargo test
```

See [PLAN.md](PLAN.md) for the full architecture and dependency breakdown.
