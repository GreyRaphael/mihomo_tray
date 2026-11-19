## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与轻量化分流数据**：`config/config.yaml` + `Country.mmdb` + `rules/*.mrs`（`private` / `category-ads` / `cn` / `category-ai-!cn` / `binance` 二进制 `.mrs` 规则集，支持 24h 自动更新且大幅降低内存与安装包体积）

### 本次更新（{{TAG}}）

- **`Profiles -> Copy URL` 订阅链接预览与一键复制**：
  - 在 `Profiles` 子菜单中新增 **`Copy URL`** 二级菜单，悬停即可预览每个订阅的链接，点击直接复制完整订阅地址到剪贴板（写入失败时弹窗提示）。
- **默认配置与菜单文案微调**：
  - 将默认模板 `config.yaml` 中的 `mixed-port` 由 `7890` 调整为 `2080`，减少常见代理端口冲突；
  - 托盘 `More` 子菜单中的开机自启项精简更名为 **`Auto Launch`**（沿用 v0.5.0 多订阅共享模板、手写原生 Win32 弹窗与 System Proxy / TUN 互斥切换特性）。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Profiles -> Set Subscription`** 填入订阅名称与链接并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` / `TUN Mode` 等状态会自动恢复）。
