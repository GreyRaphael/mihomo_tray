## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与 Loyalsoldier 分流数据**：`config/config.yaml` + `geoip.dat` + `geosite.dat` + `Country.mmdb`（已开启 24h 自动更新与国内/海外分流规则）

### 本次更新（v0.3.0）

- **优化订阅地址弹出框**：重写原生 Win32 输入弹窗，按当前显示器工作区居中定位并适配 PerMonitorV2 DPI，标题栏自动继承应用图标，采用 `Segoe UI` 字体与对称留白、`WS_EX_CLIENTEDGE` 输入框垂直居中对齐。
- **跨重启记忆运行状态**：新增便携状态文件 `config/.tray-state.json`，退出/重启时清理系统代理保证断流不影响上网，同时完整记忆退出前的 **`System Proxy` / 代理模式 / `TUN Mode`** 状态，并在下次启动或重启应用后自动恢复。
- **精简与重排托盘菜单**：
  - 根菜单：`Mihomo Status` / `Open WebUI` / `Set Subscription URL` / `Rule | Global | Direct` / `System Proxy | TUN Mode` / `Proxy Groups` / `Open` / `More` / `Quit`。
  - `More` 子菜单：`Copy Environment Variables`（复制 PowerShell 代理环境变量命令）、`Refresh Subscription`、`Restart Mihomo Core`、`Restart Application`（无互斥冲突重启应用）、`Start with Windows`、`Version: v...`。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` 等开关会自动恢复）。
