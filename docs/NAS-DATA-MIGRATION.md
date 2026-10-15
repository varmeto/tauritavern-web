# NAS data import and storage

The NAS Compose file uses two project-relative folders:

- `./data:/data`: persistent TauriTavern data in `共享文件夹/docker/tauritavern/data`.
- `./imports:/imports:ro`: read-only archive source in `共享文件夹/docker/tauritavern/imports`.

Put a `.zip`, `.tar`, `.tar.gz`, `.tgz`, or `.gz` archive in `imports` using the NAS File Manager. In TauriTavern's Data Migration panel, choose **Import Data Archive** and select the NAS file. The browser sends only the file name; the app reads the archive through its read-only mount. The import still merges into `/data` and overwrites same-path files.

## Move an existing Compose volume to the shared folder

Do not switch the app mount directly if the current project has data in its named `tt_data` volume. The old volume remains intact, but the new `./data` folder starts empty. Copy it first:

1. Create the project's `data` and `imports` folders in the NAS shared folder. Keep the current project name `tauritavern` so Compose resolves the existing `tt_data` volume.
2. Temporarily keep the app's existing `tt_data:/data` mount and add `./data:/nas-data` to that app service.
3. Add this one-shot service to the same Compose project and redeploy. It copies the existing volume without modifying it, refuses to overwrite a non-empty destination, and assigns the destination to the app's UID:

```yaml
  migrate-data:
    image: crpi-r6vmfx1s6cdrwjcm.cn-beijing.personal.cr.aliyuncs.com/varmeto/tauritavern-web-app:2.3.0-v1.2-arm64
    user: "0:0"
    restart: "no"
    entrypoint: ["/bin/sh", "-ec"]
    command:
      - |
        if [ -n "$(find /nas-data -mindepth 1 -maxdepth 1 -print -quit)" ]; then echo 'Destination is not empty; refusing to overwrite'; exit 1; fi
        cp -a /source/. /nas-data/
        chown -R 10001:10001 /nas-data
        touch /nas-data/.tt-data-volume-migrated
    volumes:
      - tt_data:/source:ro
      - ./data:/nas-data
```

4. Confirm `migrate-data` exits successfully and inspect `data` in the NAS File Manager. Stop the project.
5. Remove the temporary `migrate-data` service and the app's `tt_data:/data` plus `./data:/nas-data` entries. Replace them with `./data:/data` and `./imports:/imports:ro`; set `TT_IMPORT_DIR: /imports`. Update both images to `2.3.0-v1.2-arm64` and deploy.
6. Keep the old Docker volume until the new app has been confirmed to contain the existing characters, chats, and settings. Do not select a “delete volumes” option during cleanup.

The migration service runs as root only for the one-time copy and exits. The application continues to run as UID/GID `10001:10001`.
