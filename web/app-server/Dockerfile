FROM --platform=$BUILDPLATFORM rust:1.90-bookworm AS build

ARG TARGETARCH

WORKDIR /workspace
ENV CARGO_TARGET_DIR=/workspace/target
COPY . .
RUN --mount=type=cache,id=tt-web-cargo-target-${TARGETARCH},target=/workspace/target \
    --mount=type=cache,id=tt-web-cargo-registry,target=/usr/local/cargo/registry \
    set -eux; \
    if [ "$TARGETARCH" = arm64 ]; then \
      apt-get update; \
      apt-get install --yes --no-install-recommends cmake gcc-aarch64-linux-gnu g++-aarch64-linux-gnu; \
      rustup target add aarch64-unknown-linux-gnu; \
      printf '%s\n' \
        'set(CMAKE_SYSTEM_NAME Linux)' \
        'set(CMAKE_SYSTEM_PROCESSOR aarch64)' \
        'set(CMAKE_C_COMPILER /usr/bin/aarch64-linux-gnu-gcc)' \
        'set(CMAKE_CXX_COMPILER /usr/bin/aarch64-linux-gnu-g++)' \
        'set(CMAKE_FIND_ROOT_PATH /usr/aarch64-linux-gnu)' \
        'set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)' \
        'set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)' \
        'set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)' \
        > /tmp/aarch64-toolchain.cmake; \
      export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc; \
      export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc; \
      export CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++; \
      export AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar; \
      export CMAKE_TOOLCHAIN_FILE=/tmp/aarch64-toolchain.cmake; \
      cargo build --manifest-path web/app-server/Cargo.toml --locked --release --target aarch64-unknown-linux-gnu; \
      binary=/workspace/target/aarch64-unknown-linux-gnu/release/tt-web-host; \
    else \
      cargo build --manifest-path web/app-server/Cargo.toml --locked --release; \
      binary=/workspace/target/release/tt-web-host; \
    fi; \
    mkdir -p /workspace/output; \
    cp "$binary" /workspace/output/tt-web-host

FROM debian:bookworm-slim AS app

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --create-home ttweb

WORKDIR /app
COPY --from=build /workspace/output/tt-web-host /usr/local/bin/tt-web-host
COPY default/content/presets /app/default/content/presets
ENV PORT=3093 TT_DATA_DIR=/data
USER 10001:10001
EXPOSE 3093
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD /usr/local/bin/tt-web-host --healthcheck || exit 1
ENTRYPOINT ["/usr/local/bin/tt-web-host"]
