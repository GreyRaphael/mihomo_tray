## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与轻量化分流数据**：`config/config.yaml` + `Country.mmdb` + `rules/*.mrs`（`private` / `category-ads` / `cn` / `category-ai-!cn` / `binance` 二进制 `.mrs` 规则集，支持 24h 自动更新且大幅降低内存与安装包体积）

### 本次更新（{{TAG}}）

- **新增 `binance` 二进制规则集与 `🚑Fallback` 专线分流**：
  - 在默认 `config/config.yaml` 的 `rule-providers` 中新增 `binance`（`MetaCubeX/meta-rules-dat` `binance.mrs`，24h 自动更新），并在 `rules` 中紧接 `ai` 规则后配置 `- RULE-SET,binance,🚑Fallback`，使 Binance 相关域名优先通过自动故障转移策略组 `🚑Fallback` 稳定代理。
  - 在 GitHub Release 打包流程中预下载 `./config/rules/binance.mrs` 随压缩包一同分发，并补充配置单测校验；沿用 `v0.3.4` 的国内加密 DoH + `GEOIP,CN,DIRECT,no-resolve` 与 `Update Subscription` 相对时间菜单交互。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` 等开关会自动恢复）。
