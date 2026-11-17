## mihomo-tray {{TAG}}

Windows 10/11 平台下**开箱即用、100% 绿色便携（Portable）** 的 [mihomo](https://github.com/MetaCubeX/mihomo) 系统托盘控制程序，基于 **Rust 1.100**（`edition = "2024"`）构建，无安装依赖、无常驻 Windows 服务、不写入 `%APPDATA%`。

---

### {{TAG}} 内置便携包组件

- **托盘主程序**：`mihomo-tray.exe`（已嵌入应用图标与 DPI 清单）
- **mihomo 内核**：`core/mihomo-windows-amd64-v3.exe`（`{{MIHOMO_TAG}}`）
- **WebUI 面板**：`ui/`（`Zephyruso/zashboard` `{{ZASHBOARD_TAG}}` `dist-no-fonts`）
- **默认配置与轻量化分流数据**：`config/config.yaml` + `Country.mmdb` + `rules/*.mrs`（`private` / `category-ads` / `cn` / `category-ai-!cn` 二进制 `.mrs` 规则集，支持 24h 自动更新且大幅降低内存与安装包体积）

### 本次更新（{{TAG}}）

- **默认配置升级国内 DoH 与无解析分流**：
  - 在 `config/config.yaml` 中将 `direct-nameserver`、`proxy-server-nameserver`、`nameserver` 与 `nameserver-policy` 升级为国内加密 DoH（`dns.alidns.com` / `doh.pub`，配合 `default-nameserver: [223.5.5.5, 119.29.29.29]`），防止运营商 UDP 53 劫持，同时保证首次 `Set Subscription URL` 冷启动直连秒级解析写盘；
  - 在 `RULE-SET,cn,DIRECT` 前补充 `DOMAIN-SUFFIX,cn,DIRECT`，并将 `GEOIP,CN,DIRECT` 调整为 `GEOIP,CN,DIRECT,no-resolve`，使海外域名不再触发本地 DNS 查询，直接交由代理节点远端解析以消除明文 DNS 泄露与污染误判；保留 `🤖AI` 分组原配置（含 `Local_HTTP_1080`）。
- **菜单交互与更新时间展示优化**：
  - 将 `More -> Refresh Subscription` 改名为 `Update Subscription`，并直接内联显示距上次成功更新的相对时间（`just now` / `Xm ago` / `Xh ago` / `Xd ago`），配合 `MenuFingerprint` 按粗粒度时间桶防抖动刷新；控制器不可达时自动清理过期订阅时间戳。
- **文档完善**：在中英文 README 中补充 32×32 双轴托盘图标外圈颜色（灰 / 蓝 / 橙 / 绿）与中心圆点颜色（白 / 红 / 深炭灰 / 隐藏）的完整状态映射表。

---

### 下载与使用

1. 下载 **`mihomo-tray-windows-amd64.zip`** 并解压到任意文件夹。
2. 双击运行 **`mihomo-tray.exe`**。
3. 右键点击托盘图标，选择 **`Set Subscription URL`** 填入你的订阅地址并保存，即可开始使用（左键单击托盘图标或点击 `Open WebUI` 打开控制面板）。
4. 日后如需升级或替换内核/WebUI，先通过 `Quit` 退出托盘（停止内核），直接替换 `core/` 或 `ui/` 目录文件后再重新打开 `mihomo-tray.exe` 即可（上次的 `System Proxy` 等开关会自动恢复）。
