## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与 Loyalsoldier 分流数据**：`config/config.yaml` + `geoip.dat` + `geosite.dat` + `Country.mmdb`（已开启 24h 自动更新与国内/海外分流规则）

### 本次更新（{{TAG}}）

- **优化默认 `config/config.yaml` 策略组与分流规则**：
  - 为 `Auto` 与 `Fallback` 策略组补充 `timeout: 3000`、`lazy: true`、`max-failed-times: 3` 参数，减少空闲测速开销并降低偶发丢包时的频繁切换；
  - 新增 **`🤖AI`** 策略组（`include-all: true`、`exclude-filter: "HK|MO|CN"`），配合 **`GEOSITE,category-ai-!cn,🤖AI`** 将 OpenAI / Gemini / Claude / Grok 等海外 AI 服务自动路由至非港澳大陆节点；
  - 新增 **`GEOSITE,category-ads-all,REJECT`** 拦截常见广告与隐私追踪域名；
  - 新增 **`Local_HTTP_1080`**（`127.0.0.1:1080`）本地节点与 **`HTTP_1080_Node`** 策略组便于按需切换；
  - 默认启用 `allow-lan: true`，保持 `MATCH,Proxy` 与 `geosite:geolocation-!cn` 统一出口调度。
- **沿用核心托盘能力**：支持带图标 DPI 居中对齐的 `Set Subscription URL` 订阅输入弹窗（默认 12h 更新）、跨重启状态持久化（`config/.tray-state.json`）、悬停防自动关闭以及完整 `More` 工具子菜单。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` 等开关会自动恢复）。
