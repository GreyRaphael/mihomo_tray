# mihomo-tray

English | [中文](README_zh.md)

A lightweight, **100% portable** Windows 10/11 system tray controller for the [mihomo](https://github.com/MetaCubeX/mihomo) core, built on Rust 1.100 (`edition = "2024"`) and modern `crates.io` libraries with clean platform boundaries for future Linux/macOS support.

## Highlights

- **Fully Portable**: All configs, mihomo geodata/cache (`-d`), logs, and optional WebUI files stay inside the executable directory (`portable_root`). Relative paths in `tray.yml` resolve relative to `portable_root`.
- **Stop-and-Replace Core**: Click `Quit` to stop the core and exit, drop any updated `mihomo*.exe` into `./core/` (auto-discovered even without renaming if it is the only `mihomo*.exe`), and relaunch (previous `System Proxy`, mode, and `TUN Mode` states are restored automatically via `./config/.tray-state.json`).
- **Stop-and-Replace WebUI**: Drop any static dashboard (`zashboard`, `metacubexd`, `yacd-meta`) into `./ui/` (passed automatically as `-ext-ui`), or configure `ui.web_url` in `tray.yml`. Opens in your default browser.
- **No Service Required**: Normal execution runs non-elevated; TUN mode triggers UAC only when needed via a hidden self-helper (`--elevate-start` / `--elevate-stop`).
- **Multi-Profile Subscriptions (Single Template)**: Manage multiple subscriptions under `Profiles` stored directly in `config/config.yaml` (`proxy-providers`, cached per-profile under `./config/providers/<name>.yaml`). Switching profiles is mutually exclusive and uses local cache for instant switching; includes `Set Subscription` (native two-field Name + URL dialog), per-profile `Update Subscription` with native result dialog, and `Delete Profile` with confirmation.
- **Persistent State Across Restarts**: Remembers `System Proxy`, routing mode (`Rule` / `Global` / `Direct`), and `TUN Mode` (mutually exclusive with `System Proxy`) in `./config/.tray-state.json` across `Quit` and `More -> Restart Application` while still cleanly disabling OS proxy on shutdown.
- **Ready-to-Use Release Bundle**: GitHub Releases bundle `mihomo-tray.exe` (with embedded icon), the latest `mihomo-windows-amd64-v3` core, `zashboard` (`dist-no-fonts`), default `config/config.yaml`, `Country.mmdb`, and lightweight binary `.mrs` rule-sets (`./config/rules/*.mrs`) for low memory footprint.
- **Streamlined Menu + Procedural Icon**: Streamlined menu (`Open WebUI`, `Profiles`, modes, `System Proxy`, `TUN Mode`, `Proxy Groups`, `Open`, `More`, `Quit`), fingerprint-based rebuilds with menu-open guards so hovering never dismisses the menu, and a 32x32 procedural RGBA dual-axis icon (`outer ring = state`, `center dot = mode`).

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

## Tray Icon Colors & States

`mihomo-tray` dynamically renders a high-contrast 32×32 RGBA dual-axis tray icon ([`src/tray/icon.rs`](src/tray/icon.rs)): the **outer ring** shows the operational state (by priority), and the **center dot** shows the active routing mode.

### Outer Ring (Operational State, in Priority Order)

| Color | RGB | State | Condition & Meaning |
| :--- | :--- | :--- | :--- |
| **Gray** | `rgb(130, 140, 152)` | `Unreachable` | Controller API unreachable (core stopped, still starting, or query failed) |
| **Blue** | `rgb(33, 150, 243)` | `Tun` | `TUN Mode` enabled (takes precedence even if `System Proxy` is also enabled) |
| **Amber** | `rgb(251, 140, 0)` | `SysProxy` | `System Proxy` enabled without `TUN Mode` |
| **Green** | `rgb(46, 204, 113)` | `Running` | Core reachable with neither `System Proxy` nor `TUN Mode` enabled |

### Center Dot (Proxy Mode)

| Color | RGB | Mode | Meaning |
| :--- | :--- | :--- | :--- |
| **Light White** | `rgb(244, 247, 250)` | `Rule` | Rule-based routing |
| **Red** | `rgb(229, 57, 53)` | `Global` | Global proxy routing |
| **Dark Charcoal** | `rgb(34, 39, 46)` | `Direct` | Direct routing |
| **None (matches ring)** | — | `Unknown` | Controller unreachable or unrecognized mode (e.g. solid gray circle when unreachable) |

### Common Combinations

- **Solid Gray**: Core stopped / controller unreachable
- **Green ring + White dot**: Core running (`Rule` mode), neither System Proxy nor TUN enabled
- **Amber ring + White dot**: `Rule` mode + `System Proxy` enabled
- **Blue ring + White dot**: `Rule` mode + `TUN Mode` enabled
- Center dot switches to **Red** (`Global`) or **Dark Charcoal** (`Direct`) when changing modes.

## Build & Check

```powershell
# Windows 10/11 release build
cargo build --release

# Unit tests (cross-platform core & logic)
cargo test
```

See [PLAN.md](PLAN.md) for the full architecture and dependency breakdown.
