## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与轻量化分流数据**：`config/config.yaml` + `Country.mmdb` + `rules/*.mrs`（`private` / `category-ads` / `cn` / `category-ai-!cn` / `binance` 二进制 `.mrs` 规则集，支持 24h 自动更新且大幅降低内存与安装包体积）

### 本次更新（{{TAG}}）

- **多订阅管理（共享单份 `config.yaml` 模板）**：
  - 托盘一级菜单新增 **`Profiles`** 子菜单，平铺互斥单选所有订阅，无需额外清单文件（直接以 `config.yaml` 的 `proxy-providers` 为唯一数据源，各订阅缓存独立保存在 `./config/providers/<name>.yaml`）；
  - 切回已有本地缓存的订阅时直接基于缓存秒切；`🤖AI` 组自动改用 `include-all-proxies: true` + 单激活 `use:`，避免未激活订阅节点串入；
  - `Profiles` 下集成 **`Set Subscription`**（新增或同名更新订阅 URL）、**`Update Subscription`**（按订阅单独刷新并显示相对更新时间，成功/失败均弹窗反馈）与 **`Delete Profile`**（二次确认删除配置条目及本地缓存文件），`More` 中不再保留重复项。
- **彻底移除 `rustydialogs`，手写精简 Win32 原生弹窗**：
  - 使用 `MessageBoxW` 与内存 `DLGTEMPLATE`（`DialogBoxIndirectParamW`）原生实现提示框、确认框与居中 `Name + Subscription URL` 双输入框，并自带标题栏/任务栏图标，零额外第三方弹窗依赖。
- **系统代理与 TUN 互斥切换及菜单细节打磨**：
  - `System Proxy` 与 `TUN Mode` 调整为互斥切换（开启其一自动关闭另一项，兼容旧版双开状态恢复，取消 UAC 提权时保留原有系统代理）；
  - 状态首行改为 `Mihomo <version> Running(<port>)` 格式；`Start with Windows` 精简更名为 `Auto Start`。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Profiles -> Set Subscription`** 填入订阅名称与链接并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` / `TUN Mode` 等状态会自动恢复）。
