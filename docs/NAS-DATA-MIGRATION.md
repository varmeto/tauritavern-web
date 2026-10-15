# Data import and Docker storage

The NAS Compose file uses two project-relative folders:

- `./data:/data`: persistent TauriTavern data. For example, a NAS project at `共享文件夹/docker/tauritavern` stores it in `共享文件夹/docker/tauritavern/data`.
- `./imports:/imports:ro`: optional, read-only archive source. In that example, use `共享文件夹/docker/tauritavern/imports`.

The Data Migration panel offers two choices:

- **Upload archive from this device:** select an archive in the browser. The file is transferred in 8 MiB chunks; failed chunks are retried. The default maximum is 4 GiB, configurable with `TT_WEB_IMPORT_MAX_BYTES` (bytes). Large uploads may still be blocked by a reverse proxy; use the server folder choice in that case.
- **Import archive from server folder:** put a `.zip`, `.tar`, `.tar.gz`, `.tgz`, or `.gz` file directly in the host `imports` folder, then choose it in the panel. Only the file name crosses the browser connection. If `/imports` is not mounted, this choice reports that the folder is unavailable; browser upload still works.

Both choices create the same import job. The archive is staged before applying changes, so ensure `/data` and the container's temporary storage have room for the archive and extraction. Import merges into `/data` and overwrites same-path files. Back up `/data` first. Do not delete or replace a server-folder archive while its import job is running.

## Move an existing Compose volume to the shared folder

Do not switch the app mount directly if the current project has data in its named `tt_data` volume. The old volume remains intact, but the new `./data` folder starts empty. Copy it first:

1. Create the project's `data` and `imports` folders in the NAS shared folder. Keep the current project name `tauritavern` so Compose resolves the existing `tt_data` volume.
2. Temporarily keep the app's existing `tt_data:/data` mount and add `./data:/nas-data` to that app service.
3. Add this one-shot service to the same Compose project and redeploy. It copies the existing volume without modifying it, refuses to overwrite a non-empty destination, and assigns the destination to the app's UID:

```yaml
  migrate-data:
    image: crpi-r6vmfx1s6cdrwjcm.cn-beijing.personal.cr.aliyuncs.com/varmeto/tauritavern-web-app:2.3.0-v1.3-arm64
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
5. Remove the temporary `migrate-data` service and the app's `tt_data:/data` plus `./data:/nas-data` entries. Replace them with `./data:/data` and `./imports:/imports:ro`; set `TT_IMPORT_DIR: /imports`. Update both images to `2.3.0-v1.3-arm64` and deploy.
6. Keep the old Docker volume until the new app has been confirmed to contain the existing characters, chats, and settings. Do not select a “delete volumes” option during cleanup.

The migration service runs as root only for the one-time copy and exits. The application continues to run as UID/GID `10001:10001`.
