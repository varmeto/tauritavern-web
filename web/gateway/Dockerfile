FROM --platform=$BUILDPLATFORM node:24-bookworm-slim AS frontend-build

WORKDIR /workspace
RUN corepack enable && corepack prepare pnpm@11.22.0 --activate
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
RUN pnpm run web:build && pnpm run web:gateway:test

FROM node:24-alpine AS gateway

WORKDIR /app
ENV NODE_ENV=production PORT=8080
COPY --from=frontend-build /workspace/src ./src
COPY web/gateway ./web/gateway
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 \
  CMD node -e "fetch('http://127.0.0.1:8080/healthz').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"
CMD ["node", "web/gateway/server.mjs"]
