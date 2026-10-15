# TauriTavern Web v1.1 deployment from Alibaba Cloud ACR

This Compose file pulls the ARM64 app and gateway images from the public Beijing ACR Personal Edition repositories.

## Deploy on UGREEN NAS

1. In Docker, choose **Project → Create**, then paste this file into the Compose editor.
2. Replace `CHANGE_ME` in `TT_WEB_PASSWORD` with a unique password of at least 12 characters. Do not publish the real password.
3. Keep the app service internal. Only gateway publishes NAS port `40377` to container port `8080`.
4. Start the project and open `http://192.168.31.156:40377` on the LAN.
5. The named volume `tt_data` is new and starts empty. Restore an existing Tavern export after login if needed.

The two images are separate because Compose runs the app and gateway as separate services. The app port 3093 is not published to the NAS host.

## Images

- `crpi-r6vmfx1s6cdrwjcm.cn-beijing.personal.cr.aliyuncs.com/varmeto/tauritavern-web-app:2.3.0-v1.1-arm64`
- `crpi-r6vmfx1s6cdrwjcm.cn-beijing.personal.cr.aliyuncs.com/varmeto/tauritavern-web-gateway:2.3.0-v1.1-arm64`

## Source

Corresponding source: https://github.com/varmeto/tauritavern-web
