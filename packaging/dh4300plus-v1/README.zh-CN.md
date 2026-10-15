# TauriTavern Web 2.3.0-v1.1：DH4300 Plus 升级

本包有 app 和 gateway 两个 ARM64 镜像。只发布 gateway 的宿主机端口 `40377`；app 仍只在 Compose 私有网络中。镜像不含聊天、角色卡、密钥或密码。

## 升级前

1. 在正在运行的 NAS Compose 项目中查看 app 的 `/data` 挂载，记下真实 Docker 卷名。也可在 NAS 终端运行 `docker inspect tauritavern-web-app-1 --format '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Name}}{{end}}{{end}}'`。不要凭目录名猜卷名。
2. 在酒馆内导出完整数据包，并把导出包留在 NAS 之外。再备份该 Docker 卷。例如在旧 app 镜像仍存在时：`docker run --rm --mount type=volume,source=实际卷名,target=/data,readonly --mount type=bind,src="$PWD",dst=/backup --entrypoint tar tauritavern-web-app:2.3.0-v1-arm64 -czf /backup/tauritavern-data-before-v1.1.tar.gz -C /data .`。确认压缩包非空。
3. 核对本包 `SHA256SUMS.txt`；在 NAS 终端可执行 `sha256sum -c SHA256SUMS.txt`。

## 导入与启动

1. 导入镜像包：`docker load -i tauritavern-web-2.3.0-v1.1-arm64.tar`。
2. 将 `compose.yaml`、`.env.example` 放进 NAS 上原 Compose 项目目录。复制 `.env.example` 为 `.env`，填入原访问密码 `TT_WEB_PASSWORD`，把 `TT_DATA_VOLUME` 设为第 1 步确认的真实卷名。不要更改正在使用的端口 `40377`。
3. 先运行 `docker compose config`，确认 app 的卷显示为第 1 步的真实名称，且只有 gateway 发布 `40377:8080`。再运行 `docker compose up -d`。
4. 用原局域网地址 `http://192.168.31.156:40377` 或原有 HTTPS 域名登录。检查角色、聊天、扩展及设置，再做一轮模型生成、锁屏返回和刷新续接。

`TT_DATA_VOLUME` 未设置时 Compose 会拒绝启动，防止误建空数据卷。不要在升级过程中运行 `docker compose down -v`。

## 回退

把 `.env` 中的 `TT_RELEASE_TAG` 改回 `2.3.0-v1-arm64`，执行 `docker compose up -d`。如果新版本已修改数据格式，先停止容器，再从升级前的数据包或卷备份恢复，然后启动旧镜像。保留本包和旧镜像，直到 NAS 真机验收完成。
