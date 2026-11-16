## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与 Loyalsoldier 分流数据**：`config/config.yaml` + `geoip.dat` + `geosite.dat` + `Country.mmdb`（已开启 24h 自动更新与国内/海外分流规则）

### 本次更新

- **新增托盘直接配置订阅**：右键菜单新增 `Set Subscription URL...`，输入订阅地址即可写入 `config/config.yaml`（默认每 12 小时自动更新，并自动关联至 `Proxy` / `Auto` / `Fallback` 策略组且立即拉取节点）。
- **修复悬停菜单自动关闭 Bug**：增加 `MenuFingerprint` 变更比对与 Win32 菜单状态检测（`GetGUIThreadInfo`），后台轮询不再在菜单展开期间销毁并重建 `HMENU`。
- **简化退出菜单**：合并为单一 `Quit` 菜单项（默认停止受管内核并关闭系统代理）。
- **嵌入可执行文件图标**：基于双轴状态图标生成 `assets/app.ico`，打包至 `mihomo-tray.exe`。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL...`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先点击 `Core -> Stop Core` 停机后直接替换 `core/` 或 `ui/` 目录文件即可。
