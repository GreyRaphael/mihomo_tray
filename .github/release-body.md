## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与轻量化分流数据**：`config/config.yaml` + `Country.mmdb` + `rules/*.mrs`（`private` / `category-ads` / `cn` / `category-ai-!cn` / `binance` 二进制 `.mrs` 规则集，支持 24h 自动更新且大幅降低内存与安装包体积）

### 本次更新（{{TAG}}）

- **无损配置与单实例 / 日志轮转加固**：
  - 引入 `yaml-edit` 无损更新 `config.yaml` 订阅配置，保留用户原有注释与缩进格式，并防止误覆盖非 UTF-8 或暂时不可读配置文件；
  - 改为全局单实例互斥锁并在持锁后初始化 `2 MB` 自动轮转日志（`file-rotate`），支持 `Restart Application` 平滑重拉与外部核心 `-f` 配置自动识别。
- **TUN UAC 自提权链路与系统代理稳定性提升**：
  - 完善按需 UAC 自提权（`--elevate-start` / `--elevate-stop`）：支持自定义内核路径与 `CommandLineToArgvW` 安全引号转义、提权目标白名单校验、提权后二次下发校验 TUN 并自动恢复路由模式；
  - 合并写入 Windows `ProxyOverride`（保留用户已有规则与 `<local>`），控制器 HTTP 客户端禁用系统代理防止自环，修复核心异常退出/控制器未配置时的状态污染与空等。
- **托盘菜单交互稳定化与内部架构精简**：
  - 通过 `BTreeMap` 稳定化 + `MenuFingerprint` 哈希指纹 + 代际菜单 ID（`act_{generation}_{index}`）与弹出菜单防打断延迟重建，消除菜单闪烁与跨代误触；
  - 异步化慢速测速与订阅重载并简化 `worker` / `CoreLifecycle` 架构边界，统一使用 `rustydialogs` / `arboard` / `which` / `url` / `time` 等成熟生态组件。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` 等开关会自动恢复）。
