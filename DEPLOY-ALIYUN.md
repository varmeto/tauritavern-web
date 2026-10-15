# TauriTavern Web 2.3.0-v1.3 deployment

The public Alibaba Cloud ACR repositories provide separate app and gateway images. Use `compose.nas-aliyun.yaml` on an ARM64 NAS. The app runs only inside the Compose network; gateway publishes host port `40377` to container port `8080`.

## Before deployment

1. Back up the current app `/data` mount. Inspect its real source with `docker inspect <app-container> --format '{{json .Mounts}}'`. Keep that exact source when upgrading. Do not run `docker compose down -v`.
2. Put the Compose file in the intended project directory. `./data` and `./imports` are relative to the Compose project directory, which may differ from the directory shown by a NAS file picker. If the NAS Docker UI stores projects elsewhere, replace these with verified absolute host paths. Create both folders and ensure the app container user (UID 10001) can write to `data`; `imports` only needs read access.
3. Replace `CHANGE_ME` with a unique password of at least 12 characters. Do not commit or share the real password. `TT_WEB_IMPORT_MAX_BYTES` controls the browser upload limit in bytes; the default shown is 4 GiB.
4. Review `docker compose config` or the NAS project preview. Confirm the app has the original data mount and no host port; confirm only gateway publishes `40377:8080`.

Deploy and open `http://<NAS_IP>:40377` on the LAN, or use your existing HTTPS reverse proxy. The proxy and router port settings do not need to change for this release.

## Import data

In **Extensions → Data Migration**, choose either **Upload archive from this device** or **Import archive from server folder**. For the second choice, put the archive directly inside the host `imports` folder. The server mounts it read-only at `/imports`. Both choices merge into the persistent `/data` directory and overwrite files at the same path; back up `/data` first. Browser upload uses small retried chunks and is limited to 4 GiB by default. See `docs/NAS-DATA-MIGRATION.md` for the storage migration steps.

## Images and source

- `crpi-r6vmfx1s6cdrwjcm.cn-beijing.personal.cr.aliyuncs.com/varmeto/tauritavern-web-app:2.3.0-v1.3-arm64`
- `crpi-r6vmfx1s6cdrwjcm.cn-beijing.personal.cr.aliyuncs.com/varmeto/tauritavern-web-gateway:2.3.0-v1.3-arm64`
- Corresponding source: https://github.com/varmeto/tauritavern-web

The same repositories also provide `2.3.0-v1.3-amd64` tags for x86-64 Docker hosts. Do not replace the existing images or restore data until the new app has passed login, import, chat and persistence checks on your device.
