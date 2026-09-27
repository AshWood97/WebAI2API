# WebAI2API-RS 镜像。构建上下文是原仓库根目录：
#   docker build -f WebAI2API-RS/Dockerfile .
# 第一阶段编译 Rust 二进制，第二阶段用 Node 运行时承载引擎桥和浏览器内核。

FROM rust:1.85-bookworm AS rust-build
WORKDIR /src
COPY WebAI2API-RS/Cargo.toml WebAI2API-RS/Cargo.lock ./
COPY WebAI2API-RS/src ./src
COPY WebAI2API-RS/bridge ./bridge
COPY WebAI2API-RS/scripts ./scripts
RUN cargo build --release

FROM node:22-bookworm AS webui-build
WORKDIR /webui
COPY webui/package.json webui/package-lock.json ./
RUN npm ci --ignore-scripts
COPY webui/ ./
RUN npm run build

FROM node:22-bookworm
WORKDIR /app

ENV DEBIAN_FRONTEND=noninteractive
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=true
ENV CAMOUFOX_INSTALL_DIR=/app/camoufox
# 二进制装在 /usr/local/bin；桥位于 /app 下，以便 ESM 解析 /app/node_modules。
ENV WEBAI2API_HOME=/app/rs

# 浏览器运行库与虚拟显示，同原仓库 Dockerfile
RUN apt-get update && apt-get install -y \
    xvfb x11vnc xz-utils \
    libasound2 libatk-bridge2.0-0 libatk1.0-0 libgtk-3-0 \
    libnss3 libnspr4 libx11-xcb1 libxss1 libxtst6 libgbm1 \
    libdbus-glib-1-2 libxkbcommon0 libxcomposite1 libxdamage1 \
    libxrandr2 libxfixes3 libxext6 libpango-1.0-0 libcairo2 \
    libcups2 libdrm2 libexpat1 \
    python3 build-essential \
    && rm -rf /var/lib/apt/lists/*

# 原仓库源码与依赖：引擎桥直接 import 它们，不复制代码
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY src ./src
COPY scripts ./scripts
COPY patches ./patches
RUN npm install -g pnpm && pnpm install --frozen-lockfile && npm run init \
    && rm -rf /app/src

COPY --from=rust-build /src/target/release/webai2api /usr/local/bin/webai2api
COPY --from=rust-build /src/target/release/webai2api-genkey /usr/local/bin/webai2api-genkey
# 桥、浏览器 SDK 辅助模块、脚本与 WebUI 放在独立目录。
COPY WebAI2API-RS/bridge /app/rs/bridge
COPY WebAI2API-RS/utils /app/rs/utils
COPY WebAI2API-RS/scripts /app/rs/scripts
COPY --from=webui-build /webui/dist /app/rs/webui-dist

EXPOSE 3000 5900

# --src-root 指向本目录（含 src/ 与 camoufox/），数据在 /app/data
CMD ["webai2api", "--src-root", "/app", "--data-dir", "/app/data", "-xvfb", "-vnc"]
