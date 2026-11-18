# mihomo-tray Implementation Plan (Win10+ Portable, Cross-Platform Ready)

## 1. Goals & Design Principles

`mihomo-tray` is a lightweight, **100% portable** system tray controller for the **mihomo** core, built with **Rust 1.100** (`edition = "2024"`).

### Explicit Scope & Constraints
1. **Windows 10/11 First (Win10+)**: Target Windows 10/11 directly with zero Linux/macOS runtime compromises today, while cleanly isolating OS-specific boundaries so Linux and macOS backends can be added later without rewriting core logic.
2. **100% Portable (No Installer, No Global State Pollution)**:
   - All runtime files (`tray.yml`, core binary, mihomo home/config/geodata/cache, logs, and optional external WebUI) stay inside the portable root directory (the executable's directory on Windows).
   - Relative paths in `tray.yml` are first-class citizens and are resolved relative to `portable_root`, then canonicalized/expanded to absolute paths before passing `-d` / `-f` / `-ext-ui` to `mihomo`.
   - Never installs Windows Services; TUN privilege elevation uses a hidden self-elevation helper (`--elevate-*`) via UAC.
3. **English-Only UI**: Static English copy and error messages (`src/text.rs`); no runtime i18n loader or locale probing overhead.
4. **No Embedded/Hosted WebUI (Offline-Swappable on Stop)**:
   - Does not embed frontend HTML/JS or run an internal HTTP server.
   - Users can drop any WebUI distribution (e.g., `zashboard`, `metacubexd`, `yacd-meta`) into `./ui/` and replace it anytime while stopped, or configure `ui.web_url` in `tray.yml` (with `{host}`, `{port}`, `{secret}` placeholders).
   - Opens WebUI via the **system default browser** (`open` crate) — no Edge `--app` wrapper or WebView2 runtime management.
5. **Swappable `mihomo` Core (Stop-and-Replace Friendly)**:
   - Users can click `Quit` to stop the managed kernel cleanly, replace `mihomo*.exe` in `./core/` (or beside the tray binary), and relaunch (previous `System Proxy`, mode, and `TUN Mode` states restore automatically via `.tray-state.json`), or click `More -> Restart Mihomo Core` to rescan and restart.
   - Auto-discovery supports both exact `mihomo.exe` and a single unambiguous `mihomo*.exe` inside `./core/` or `./`.
6. **No Dark/Light Theme Polling**: Fixed high-contrast procedural RGBA tray icons (outer ring = operational state, inner dot = proxy mode).
7. **Mature `crates.io` Ecosystem**: Prefer latest stable `crates.io` crates over hand-rolled Win32 HTTP/PEB/YAML parsers or Git forks.

---

## 2. Portable Directory Layout

```text
mihomo-tray/
├── mihomo-tray.exe       # Tray controller (+ hidden --elevate-* helper)
├── tray.yml              # Portable tray configuration (created on first run)
├── core/                 # Place mihomo.exe or any single mihomo-*.exe here
│   └── mihomo.exe
├── config/               # Default mihomo -d directory (geodata, cache.db, rulesets)
│   ├── config.yaml       # Default mihomo -f configuration file
│   └── .runtime.json     # Ephemeral PID / controller lockfile while core is managed
├── ui/                   # Optional user-supplied static WebUI (passed via -ext-ui)
│   └── index.html
└── logs/
    └── mihomo-tray.log   # Local portable log file
```

---

## 3. Dependency Stack (Latest Stable `crates.io` Versions on Rust 1.100)

### 3.1 Cross-Platform Core Dependencies
| Crate | Version | Purpose |
|---|---|---|
| `tray-icon` | `0.26` | System tray icon & click events (`muda-common-controls-v6` on Windows; `ksni` ready on Linux) |
| `muda` | `0.21` | Native tray menu (`0.21.1+` includes Windows 16-bit command ID recycling, no fork needed) |
| `winit` | `0.30` | Event loop via `ApplicationHandler<AppEvent>` + `EventLoopProxy` for zero-spin wakeups |
| `ureq` | `3.5` | Synchronous local HTTP/1.1 client (`default-features = false, features = ["json"]`, no TLS/Tokio) |
| `serde` | `1.0` | Serialization/deserialization framework (`derive`) |
| `serde_json` | `1.0` | Mihomo REST API payload parsing |
| `serde_yaml_ng` | `0.10` | Typed YAML inspection for `tray.yml` and `config.yaml` |
| `yaml-edit` | `0.3` | Lossless CST-based YAML updates preserving user comments/formatting in `config.yaml` |
| `url` / `percent-encoding` | `2.5` / `2.3` | Controller/subscription URL parsing & query/path encoding |
| `time` | `0.3` | RFC 3339 timestamp parsing for provider `updatedAt` |
| `sysinfo` | `0.40` | Cross-platform process inspection (`default-features = false, features = ["system"]`) |
| `which` | `8.0` | Cross-platform `PATH` kernel lookup (with Scoop `.shim` resolution on Windows) |
| `open` | `5.4` | Open WebUI URL, config files, and portable folders in system default apps |
| `arboard` | `3.7` | Cross-platform clipboard copy (`Copy Environment Variables`) |
| `rustydialogs` | `0.5` | Native message dialogs, Yes/No confirmations, and subscription input prompts |
| `single-instance` | `0.3` | Global single-instance mutex (`mihomo-tray`) |
| `auto-launch` | `0.6` | Cross-platform startup registration (`Start with Windows`) |
| `file-rotate` | `0.8` | Size-based log rotation (`2 MB`) in `./logs/` |
| `tracing` | `0.1` | Structured diagnostic logging |
| `tracing-subscriber` | `0.3` | Formatting logger backed by `file-rotate` |

### 3.2 Windows-Only Dependencies (`[target.'cfg(windows)']`)
| Crate | Version | Purpose |
|---|---|---|
| `winresource` | `0.1` | `build.rs` single-pass embedding of `assets/app.ico` + DPI/UTF-8 manifest (avoids `CVT1100` conflicts) |
| `winreg` | `0.56` | Per-user `Internet Settings` registry updates |
| `windows-sys` | `0.61` | `InternetSetOptionW`, `ExpandEnvironmentStringsW`, `ShellExecuteExW` (`CommandLineToArgvW`-safe quoting), `GetGUIThreadInfo` |

> **Implementation note**: `ShellExecuteExW` via `windows-sys` is used directly for UAC elevation instead of `runas 1.2.0`, because `runas 1.2.0` over-escapes backslashes inside quoted Windows paths containing spaces and leaks the child process handle.

---

## 4. Architecture & Cross-Platform Boundary

```text
src/
├── main.rs               # Helper dispatch -> single-instance -> EventLoop<AppEvent>
├── app.rs                # winit ApplicationHandler, tray icon/menu orchestration, 300ms debounce
├── text.rs               # English UI strings & formatted messages
├── state.rs              # Snapshot, Group, Provider, WorkerCommand, AppEvent
├── config/
│   ├── mod.rs
│   ├── tray.rs           # TraySettings + portable relative path resolution
│   ├── mihomo.rs         # Mihomo YAML inspection (external-controller, secret, external-ui)
│   └── state_store.rs    # PersistedState (.tray-state.json) across app restarts
├── core/
│   ├── mod.rs
│   ├── api.rs            # Mihomo REST client (ureq 3.5)
│   ├── discover.rs       # Kernel binary, config/home/ui, controller resolution & launch args
│   ├── process.rs        # .runtime.json lockfile + sysinfo process picking & termination
│   └── worker/           # Background polling & command execution thread
│       ├── mod.rs
│       ├── config_tracker.rs
│       ├── lifecycle.rs
│       ├── state_sync.rs
│       └── tasks.rs
├── tray/
│   ├── mod.rs
│   ├── icon.rs           # Procedural RGBA icon renderer (32x32, outer ring + center mode dot)
│   └── menu.rs           # muda 0.21 menu builder with hash fingerprint diffing, Action dispatch & paging
└── platform/
    ├── mod.rs            # Platform abstraction boundary + rustydialogs/arboard/auto-launch + #[cfg] dispatch
    ├── windows.rs        # Win10+ implementation (WinINet proxy, UAC helper, Scoop shim, GUI menu probe)
    ├── win_helpers.rs    # Pure Windows helper logic (testable on non-Windows hosts)
    └── stub.rs           # Non-Windows fallback/scaffold for host testing & future Linux/macOS
```

### Why Future Linux / macOS Support Requires Minimal Refactoring
Business modules (`config`, `core`, `tray`, `app`, `state`, `text`) **never import `windows_sys` or `winreg`**. Only four OS concerns sit behind `src/platform/mod.rs`:
1. `portable_root()` & `kernel_candidates()`: Windows uses `exe.parent()` and `*.exe`; macOS can later back out of `.app/Contents/MacOS`; Linux can drop `.exe` and support symlinks.
2. `SystemProxy`: Windows uses `winreg` + `InternetSetOptionW` (preserving user-curated bypass entries and avoiding unnecessary refreshes); Linux/macOS can later plug in `gsettings`/`kconfig` and `networksetup`.
3. `PrivilegeHelper`: Windows spawns self with `--elevate-start` / `--elevate-stop` via `ShellExecuteExW` (`"runas"`); Linux can later use `pkexec setcap` / `pkexec`; macOS can use `osascript` or privileged launch.
4. `notify_error()` / `confirm_yes_no()` / `prompt_subscription_url()`: Unified cross-platform via `rustydialogs`, with graceful headless fallback on non-Windows.

---

## 5. Key Functional Flows

### 5.1 Relative Path & Discovery Chain
1. **Tray settings (`tray.yml`)**: Always located at `<portable_root>/tray.yml`. Created from `assets/tray.sample.yml` if missing.
2. **Relative paths**: Any non-empty `mihomo.path`, `mihomo.home`, `mihomo.config`, or `ui.external_ui` is `%VAR%`-expanded (on Windows) and, if relative, joined against `portable_root` and cleaned (`normalize_path`) into an absolute path before use.
3. **Kernel binary (`mihomo.path` empty)**:
   - `<portable_root>/mihomo.exe`
   - `<portable_root>/core/mihomo.exe`
   - `<portable_root>/bin/mihomo.exe`
   - Unambiguous single `mihomo*.exe` in `<portable_root>/core/`, then `<portable_root>/`
   - `PATH` entries (`mihomo.exe`, following Scoop `.shim` files if encountered)
4. **Home (`-d`) & Config (`-f`)**:
   - Explicit `mihomo.config` / `mihomo.home` take precedence.
   - Otherwise prefer `<portable_root>/config/config.yaml` (if `config/` or `config/config.yaml` exists), then `<portable_root>/config.yaml`, then `<kernel_dir>/config.yaml`, finally defaulting `-d` to `<portable_root>/config` (keeping geodata & cache portable).
5. **External UI (`-ext-ui`)**:
   - If `ui.external_ui` is set in `tray.yml`, pass `-ext-ui <resolved_dir>`.
   - Else if `<portable_root>/ui/index.html` exists and `config.yaml` does not set `external-ui` (and `mihomo.args` does not pass `-ext-ui`), automatically pass `-ext-ui <portable_root>/ui`.
   - `Open WebUI` resolves `ui.web_url` (or default `http://{host}:{port}/ui/?hostname={host}&port={port}&secret={secret}`) and opens it via `open::that`.

### 5.2 Core Lifecycle & Safe Swapping
- **Stop-and-Replace**:
  - Clicking `Quit` (or stopping the core) terminates the managed mihomo process cleanly (using UAC `--elevate-stop <pid>` if elevated) and clears `.runtime.json`, releasing Windows file locks on `./core/*.exe` and `./ui/*`.
  - Users can overwrite or swap binaries in `./core/` and UI bundles in `./ui/`, then relaunch `mihomo-tray` (which preserves `System Proxy`, routing mode, and `TUN Mode` in `.tray-state.json`) or click `More -> Restart Mihomo Core`.
- **TUN Elevation (Zero-Service)**:
  - Normal launches spawn `mihomo` without elevation.
  - When toggling TUN ON fails to stick (`tun == false` on readback) or an elevated restart is needed, the worker resolves the running kernel PID, spawns a hidden elevated copy of `mihomo-tray.exe --elevate-start <pid> <args...>` via `ShellExecuteExW`.
  - The elevated helper reads the verified executable image path from the running `<pid>` via `sysinfo` (never accepting an arbitrary executable path from CLI arguments), stops `<pid>`, and spawns the elevated kernel with explicit absolute `-d` / `-f` / `-ext-ctl` / `-secret` / `-ext-ui` arguments.

### 5.3 Menu Rebuild & Controls
1. **Menu debounce & non-blocking UI**: Tray/menu callbacks pass events over `EventLoopProxy` and debounce rapid clicks (`300ms`). All HTTP calls run on the worker thread (with slow latency tests and subscription reloads offloaded to guarded background threads).
2. **Fingerprint-guarded menu rebuilds**:
   - Worker polls `/configs`, `/proxies`, and `/providers/proxies` into a unified `Snapshot` (status line, `Open WebUI`, `Set Subscription URL`, Modes `Rule`/`Global`/`Direct`, `System Proxy`, `TUN Mode`, `Proxy Groups` with latency sorting, optional `groups.page_size` pagination, `Test Group Latency`, `Auto (Unpin)`, `Open`, `More`, and `Quit`).
   - GUI checks `MenuFingerprint` (tracking visible group items and visible node latencies) and defers rebuilds while a native popup menu is open.
3. **High-DPI procedural icons**: 32x32 RGBA circle + inner dot (`icon.rs`), updated whenever state (`Running`/`SysProxy`/`Tun`/`Stopped`/`Unreachable`) or mode (`rule`/`global`/`direct`) changes.

---

## 6. Verification Plan

1. **Linux Host Unit & Integration Tests (`cargo test`)**:
   - Portable path normalization & relative path resolution
   - YAML settings & mihomo config parsing
   - Kernel discovery priority & unambiguous `mihomo*.exe` matching
   - Controller URL normalization (`0.0.0.0`, `:port`, `https://` rejection, `{host}/{port}/{secret}` template substitution)
   - REST client (`ureq`) request/response verification against an ephemeral loopback mock HTTP server
   - Procedural RGBA icon generation invariants
2. **Windows Target Verification (`cargo check --target x86_64-pc-windows-gnu --all-targets` & `cargo clippy --target x86_64-pc-windows-gnu --all-targets`)**:
   - Verify full Windows Win10+ compilation (`windows.rs`, `build.rs`, `tray-icon`, `muda`, `winit`, `winreg`, `windows-sys`, `rustydialogs`) with zero warnings/errors.
