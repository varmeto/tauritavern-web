# TauriTavern Web 上游升级与发布流程

当前发布基线：上游 `v2.3.0` / `a1855be4a4f8b6ee7cd0374a84dbb3709c3e5375`，Web 适配从本地提交 `2904c04` 开始。上游升级单独处理，不自动覆盖 NAS 正在运行的版本。

1. 从已验收版本建 `codex/web-upstream-vX.Y.Z` 集成分支，保留当前发布提交。先更新 `web/upstream.json`、`package.json` 和锁文件，再合并或重新应用 Web 适配；不要在 `refs/repos` 下修改第三方参考仓库。
2. 逐项核对 `src/init.js`、`src/tauri-bridge.js`、`src/scripts/extensions.js`、`src/tauri/main/routes/ai-routes.js`、`src/script.js`、`src/tt-ext-sw.js`、Rust `ChatCompletionService` 和 DTO。检查命令参数、事件名、扩展加载顺序、聊天数据格式及页面挂载点。把变化和对应测试写进 `CHANGELOG-WEB.md`。
3. 运行 `pnpm run check`、`pnpm run web:gateway:test`、浏览器宿主模拟模型测试。针对真实扩展版本检查酒馆助手、消息渲染、变量与提示词扩展，另检查角色卡、聊天、预设、世界书、导入导出及扩展安装更新。
4. CI 通过后构建 x86-64 与 ARM64 候选镜像。用相同数据和扩展各测十次冷启动与重复进入；记录资源传输、宿主就绪、核心数据、扩展激活、聊天恢复和可交互时间。目标：重复进入中位数至少快 30%，静态资源重复传输少 80%，冷启动中位数恶化不超过 10%。不达标则先分析指标再发布。
5. Windows Edge 和 Android/iOS 手机 Edge 检查聊天、设置、扩展、软键盘、横竖屏、弹窗、锁屏返回、短时断网、刷新及重新登录。模拟窗口尺寸不能代替真机。用计数模拟模型确认一次生成只请求一次上游；检查多标签页、停止、异常和容器重启。
6. NAS 升级前确认实际 `/data` 卷名并备份；生成版本化 ARM64 离线包、Compose、校验、完整源码和回退说明。维持宿主端口 `40377`，只发布 gateway。NAS 启动、登录、扩展、后台生成恢复与持久化验收通过后，才将候选版标记稳定。保留旧镜像与卷备份供回退。

长期兼容通过持续适配与回归实现。未知上游或第三方扩展更新可能需要修改代码，不能仅凭构建成功视为可游玩。
