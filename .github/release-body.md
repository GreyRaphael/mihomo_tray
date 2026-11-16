## mihomo-tray {{TAG}}

Windows 10/11 平台下轻量级、**100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）与现代 `crates.io` 生态库构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 说明

- **全便携目录模型**：`tray.yml`、`core/`、`config/`、`ui/` 与 `logs/` 全部收敛于可执行文件所在目录；`tray.yml` 内支持填写相对路径，以程序根目录为基准展开后传入内核 `-d` / `-f`。
- **停机随意替换内核与 WebUI**：
  - 通过托盘 `Core -> Stop Core` 停核后可随时更换 `./core/` 下的 `mihomo*.exe`（目录下仅有单个时无需改名即可自动识别）；
  - 不内置前端页面，可将任意静态面板（如 `zashboard`、`metacubexd`、`yacd-meta`）放入 `./ui/` 自动作为 `-ext-ui` 挂载，或在 `tray.yml` 中配置 `ui.web_url`；点击 `Open WebUI` 或左键托盘图标即由默认浏览器打开。
- **两阶段菜单刷新与纯代码双轴图标**：
  - 32x32 RGBA 实时渲染图标：外圈标识运行状态（蓝 = TUN、橙 = 系统代理、绿 = 运行中、灰 = 停止/不可达），中心点标识代理模式（浅白 = Rule、红 = Global、深灰 = Direct）；
  - 两阶段重建菜单（`muda 0.21`），支持代理组延迟展示与排序、超长列表分页（`groups.page_size`）、测速、解除固定、订阅刷新与按需 UAC TUN 提权。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`**（Windows 10/11 x86_64）并解压到任意目录。
2. 将 `mihomo.exe`（或未改名的单个 `mihomo-*.exe`）放入 `./core/`，将你的 `config.yaml` 放入 `./config/`（可选将静态 WebUI 放入 `./ui/`）。
3. 双击运行 **`mihomo-tray.exe`** 即可；需要调整控制器、分组过滤或面板地址时编辑同目录 **`tray.yml`**。
