# Web 适配修改记录

## 候选发布 2.3.0-v1.2.1

- 修复 NAS 数据导入选择框：确认关闭回调明确允许弹窗关闭，避免点击“继续”后界面停住。
- Web 导入使用站内二次确认弹窗；开始处理后立即显示状态和不定进度条，随后按任务状态更新百分比。
- 本次只更新 gateway 前端镜像；app 镜像和 NAS 数据格式不变。

## 候选发布 2.3.0-v1.1（承接基线 `2904c04`）

上游固定为 `v2.3.0` / `a1855be4a4f8b6ee7cd0374a84dbb3709c3e5375`。本次没有升级上游源码。

| 适配入口 | 目的 | 上游契约与检查 |
| --- | --- | --- |
| `src/init.js`、`src/web-adapter.js`、`src/tauri-bridge.js` | 浏览器启动、认证检查、事件重连、草稿和页面恢复 | 保持 Tauri bridge 的导出与调用形状；`tests/web`、`tests/browser/startup-order.mjs` |
| `src/tauri/main/routes/ai-routes.js`、`src/tauri/main/services/ai/*` | 使用既有模型服务启动 Web 生成任务，顺序读取流 | 保持 `/api/backends/chat-completions/generate` 的 SSE 形状及桌面路径；模拟模型回归 |
| `web/app-server/src/web_generation.rs`、`web/app-server/src/main.rs` | NAS 接管模型流、去重、落盘、续读、取消与提交确认 | 原 `ChatCompletionService` 的 `start_stream`/`read_stream`/`close_stream`；`tests/web/app-server-smoke.test.mjs` |
| `web/gateway/server.mjs`、`src/tt-ext-sw.js`、`scripts/build-web-manifest.mjs` | 版本标识、状态查询、条件请求、压缩、认证后静态缓存 | 只缓存构建清单内的静态资源；`web/gateway/server.test.mjs` |
| `src/css/web-mobile.css` | 浏览器视口、软键盘和弹窗滚动 | 仅作用于 `.tt-web-host`，保留主题和第三方扩展节点 |
| `packaging/dh4300plus-v1/*`、`scripts/package-web-release.ps1` | ARM64 离线包、现有卷保护及回退 | Compose 只发布 gateway 宿主端口；先核对 NAS 实际卷名 |

第三方扩展的模块路径、全局接口、事件名、页面挂载点和数据格式没有重命名。未知扩展仍按原顺序初始化；运行中的浏览器专属脚本在页面暂停时也会暂停。新版本仍需在真实安装的扩展上回归。
