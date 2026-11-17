## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与轻量化分流数据**：`config/config.yaml` + `Country.mmdb` + `rules/*.mrs`（`private` / `category-ads` / `cn` / `category-ai-!cn` 二进制 `.mrs` 规则集，支持 24h 自动更新且大幅降低内存与安装包体积）

### 本次更新（{{TAG}}）

- **彻底修复首次设置订阅时的超时与 `providers` 未落盘问题**：
  - 修复 `src/core/api.rs` 中 `PUT /configs?force=true` 误用 `2000ms` 轮询超时导致订阅尚未下载完即报错 `timeout: global` 的问题，改为专用的 `45s` 重载超时，并在 `src/core/worker.rs` 中仅在覆盖已有缓存或首次初始化未落盘时补发刷新，同时保留重载前运行状态（`System Proxy` / 模式 / `TUN Mode`）。
  - 修复默认 `config/config.yaml` 中无代理冷启动下 `dns.fallback`（`1.1.1.1` / `8.8.8.8` DoH）在直连网络阻塞/超时导致机场订阅域名无法解析、进而 `providers/subscription.yaml` 无法下载落盘的问题；补充 `direct-nameserver` 确保直连与首次拉取订阅稳定毫秒级解析。
- **沿用低内存 `.mrs` + `Country.mmdb` 结构**：保留约 `47 MiB` 量级空载内存优化、Emoji 策略组（`🚀Proxy` / `⚡Auto` / `🚑Fallback` / `🤖AI` + `Local_HTTP_1080`）与全部托盘交互能力。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` 等开关会自动恢复）。
