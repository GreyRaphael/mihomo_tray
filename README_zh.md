# mihomo-tray

[English](README.md) | 中文

一个轻量、**100% 绿色便携（Portable）** 的 Windows 10/11 [mihomo](https://github.com/MetaCubeX/mihomo) 内核系统托盘控制程序。使用 **Rust 1.100**（`edition = "2024"`）与 `crates.io` 最新成熟生态库实现，代码按平台边界清晰分层，方便日后平滑扩展至 Linux 与 macOS。

---

## 核心特性

- **完全绿色便携**：所有配置文件（`tray.yml`、`config.yaml`）、内核缓存与 GeoData（`-d`）、运行日志（`./logs/mihomo-tray.log`）和本地 WebUI 均收敛在可执行文件所在目录；`tray.yml` 内支持直接填写相对路径，整个文件夹打包拷贝或移动盘符即可使用，不往 `%APPDATA%` 写脏数据。
- **停机随意替换内核**：点击托盘菜单 `Quit` 退出托盘（自动停止受管内核并解除文件占用）；将新版或改名后的 `mihomo*.exe` 放入 `./core/`（只要目录下只有一个 `mihomo*.exe`，甚至无需改名即可自动识别），重新打开即可启动新内核，也可随时点击 `More -> Restart Mihomo Core` 重启内核。
- **停机随意替换 WebUI**：不内置、不离线托管固定前端，零重编译更换面板：
  - 将任意静态面板（如 `zashboard`、`metacubexd`、`yacd-meta`）解压放入 `./ui/`，启动内核时检测到 `./ui/index.html` 且配置未显式指定时会自动传入 `-ext-ui`；
  - 或者直接在 `tray.yml` 中配置 `ui.web_url`（支持 `{host}`、`{port}`、`{secret}` 占位符）；
  - 点击 `Open WebUI` 或左键单击托盘图标，直接调用系统默认浏览器打开。
- **免安装、无服务、按需 TUN 提权**：绝不注册 Windows Service；日常以普通权限启动内核，仅在启用 TUN 模式且需要管理员权限时，通过隐藏自身 UAC 辅助进程（`--elevate-start` / `--elevate-stop`）拉起提权内核。
- **托盘一键填订阅**：点击菜单 `Set Subscription URL` 打开带应用图标且自动居中适配 DPI 的输入框，填入订阅地址即可写入 `config/config.yaml`（默认 12h 自动更新并绑定到 `🚀Proxy` / `⚡Auto` / `🚑Fallback` 与 `🤖AI` 分组），同时立即拉取节点。
- **跨重启状态持久化**：在 `./config/.tray-state.json` 中记住退出前的 **`System Proxy`、代理模式、TUN 模式**状态；退出时清理系统代理避免断网，下次启动或点击 `More -> Restart Application` 后自动恢复。
- **低内存开箱即用 Release 便携包**：GitHub Releases 自动打包 `mihomo-tray.exe`（内嵌可执行文件图标）、最新 `mihomo-windows-amd64-v3` 内核、最新 `zashboard`（`dist-no-fonts`）、默认 `config/config.yaml`、`Country.mmdb` 以及轻量化 `.mrs` 二进制规则集（`./config/rules/*.mrs`）。
- **两阶段菜单刷新与纯代码双轴图标**：
  - 托盘图标由代码实时生成 32x32 RGBA 图像：外圈代表运行状态（蓝 = TUN、橙 = 系统代理、绿 = 运行中、灰 = 未运行/控制器不可达），中心点代表代理模式（浅白 = Rule、红 = Global、深灰 = Direct）；
  - 菜单采用两阶段刷新并具备菜单展开防打断保护（悬停浏览不会自动消失），支持节点延迟展示与快慢排序、超长列表分页（`groups.page_size`）、测速、解除固定、订阅刷新等操作。
- **多份便携实例独立互斥**：单实例互斥锁基于便携目录路径哈希生成，不同目录放置多份便携版互不干扰，同一目录重复启动会弹窗提示。

---

## 便携目录结构

```text
mihomo-tray/
├── mihomo-tray.exe          # 托盘主程序（同时承担隐藏提权辅助角色）
├── tray.yml                 # 托盘配置（首次启动自动按模板生成）
├── core/
│   └── mihomo.exe           # 内核文件（支持放置单个未改名的 mihomo-*.exe）
├── config/
│   ├── config.yaml          # mihomo 主配置文件（-f）
│   ├── cache.db / GeoSite   # mihomo 运行目录（-d 默认锁定在 ./config）
│   ├── .tray-state.json     # 记住上次退出前的 System Proxy / 模式 / TUN 状态
│   └── .runtime.json        # 托管内核运行期间的临时 PID 记录（退出自动清理）
├── ui/                      # 可选：自行放入的静态 WebUI（如 zashboard/dist）
│   └── index.html
└── logs/
    └── mihomo-tray.log      # 运行日志
```

---

## 配置与发现优先级

所有在 `tray.yml` 中填写的相对路径（`mihomo.path`、`mihomo.home`、`mihomo.config`、`ui.external_ui`）都会先展开环境变量（`%NAME%`），再以程序所在目录为基准解析为绝对路径：

1. **内核二进制（`mihomo.path` 留空时）**：
   `./mihomo.exe` → `./core/mihomo.exe` → `./bin/mihomo.exe` → `./core/` 或 `./` 下唯一的 `mihomo*.exe` → `PATH`（支持 Scoop `.shim`）。
2. **配置目录与文件（`-d` / `-f`）**：
   `tray.yml` 显式配置 → `./config/config.yaml`（存在 `./config` 目录或该文件时）→ `./config.yaml` → 内核同级 `config.yaml` → 默认 `-d` 为 `./config`。
3. **外部面板目录（`-ext-ui`）**：
   `tray.yml` 的 `ui.external_ui` → 若 `config.yaml` 和 `mihomo.args` 均未指定且 `./ui/index.html` 存在，自动传入 `./ui`。
4. **控制器地址与密钥**：
   运行中内核命令行（`-ext-ctl` / `-secret`）→ 环境变量 `CLASH_OVERRIDE_*` → `config.yaml` → `tray.yml` 的 `controller.*` 兜底（不支持 `https://`，`0.0.0.0` / `:port` 自动规范为 `127.0.0.1`）。

---

## 托盘菜单概览

- **状态行**：显示 `Mihomo <version>: Running (<mode>)` / `Mihomo: Stopped` / `Mihomo: Controller Unreachable` 及最近错误。
- **Open WebUI**：用默认浏览器打开面板（左键单击托盘图标同样触发）。
- **Set Subscription URL**：居中适配 DPI 的弹窗输入并保存订阅地址到 `config/config.yaml`（默认 12 小时更新），并立即触发加载与刷新节点。
- **模式切换**：`Rule` / `Global` / `Direct`。
- **接管开关**：`System Proxy` / `TUN Mode`（状态跨重启记忆）。
- **Proxy Groups**：各策略组子菜单（支持 `Test Group Latency`、`Auto (Unpin)`、延迟排序与分页）。
- **Open**：快捷打开 `tray.yml`、`config.yaml`、便携根目录、`ui/` 目录及日志文件。
- **More**：
  - `Copy Environment Variables`：复制终端代理环境变量命令到剪贴板；
  - `Refresh Subscription`：立即刷新订阅/代理提供者；
  - `Restart Mihomo Core`：重新扫描并重启 mihomo 内核；
  - `Restart Application`：重启 `mihomo-tray` 应用（无单实例冲突并自动恢复运行状态）；
  - `Start with Windows`：注册/取消开机自启（基于当前便携 exe 路径）；
  - `Version: v...`：显示应用与核心版本信息弹窗。
- **Quit**：保存运行状态、停止受管内核、清理系统代理并退出托盘。

---

## 构建与检查

环境要求：Rust **1.100+**（`edition = "2024"`）。

```powershell
# Windows 10/11 发布构建
cargo build --release

# 单元测试（覆盖配置解析、路径展开、内核发现、API 客户端、菜单与图标等）
cargo test
```

详细架构设计与依赖清单请参阅 [PLAN.md](PLAN.md)。
