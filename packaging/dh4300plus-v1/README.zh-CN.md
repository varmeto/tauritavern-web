# TauriTavern Web v1：DH4300 Plus ARM64

本目录用于在 ARM64 NAS 上部署 TauriTavern Web。镜像包只包含应用程序，不包含聊天、角色卡、API Key、密码或本机 Docker 数据卷。

## 部署

1. 将 `tauritavern-web-v1-dh4300plus-arm64.tar` 导入 NAS 的 Docker 镜像管理器，或在 NAS 终端运行 `docker load -i tauritavern-web-v1-dh4300plus-arm64.tar`。
2. 将本目录的 `compose.yaml` 与 `.env.example` 放入 NAS 上同一个项目目录。
3. 复制 `.env.example` 为 `.env`，设置唯一的 `TT_WEB_PASSWORD`（至少 12 个字符）；需要时修改 `TT_WEB_PORT`。
4. 在该目录启动 Compose 项目。
5. 用浏览器访问 `http://NAS局域网地址:8081`，端口按 `.env` 设置调整。

只有网关发布宿主机端口；应用服务仅在 Compose 私有网络中可达。命名卷 `tauritavern-data` 保存 NAS 上的聊天和设置。首次部署不会自动带入电脑里的数据；部署后通过应用的数据导入功能迁移。

升级前先用应用导出备份，并保留 NAS 上的 `tauritavern-data` 卷。不要删除该卷。
