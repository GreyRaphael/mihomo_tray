## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与轻量化分流数据**：`config/config.yaml` + `Country.mmdb` + `rules/*.mrs`（`private` / `category-ads` / `cn` / `category-ai-!cn` 二进制 `.mrs` 规则集，支持 24h 自动更新且大幅降低内存与安装包体积）

### 本次更新（{{TAG}}）

- **大幅降低内核内存占用**：从加载全量 `geosite.dat` / `geoip.dat` 切换为 **`Country.mmdb` + `.mrs` 二进制规则集 + `MATCH,🚀Proxy` 漏网之鱼兜底**（空载 `VmRSS` 从约 `108.7 MiB` 降至约 `47 MiB` 量级）。
- **优化策略组与规则结构**：
  - 为 `🚀Proxy`、`⚡Auto`、`🚑Fallback`、`🤖AI` 分组统一添加 Emoji 前缀，补充 `timeout: 3000`、`lazy: true`、`max-failed-times: 3`；
  - `🚀Proxy` 可直接选择 **`⚡Auto`**、**`🚑Fallback`**、**`Local_HTTP_1080`**（`127.0.0.1:1080`）、`DIRECT` 或手动点选订阅节点；
  - `category-ads.mrs` 拦截简单广告，`private` 与 `cn` 全走 `DIRECT`，`🤖AI`（排除 `HK|MO|CN`）承接海外 AI 流量，其余走 `🚀Proxy`。
- **DNS 防污染精简**：使用 `proxy-server-nameserver` + `rule-set` 国内分流与 `fallback-filter(CN)`（不再重复加载 `geosite.dat`）。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` 等开关会自动恢复）。
