FROM node:22-bookworm

WORKDIR /app

ENV DEBIAN_FRONTEND=noninteractive
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=true
# camoufox-js@0.12 在模块加载时读取该变量（必须指向项目内 camoufox 目录）
ENV CAMOUFOX_INSTALL_DIR=/app/camoufox

# 1. 安装系统依赖
# Clearcote (Chromium) 官方 Node 文档要求 xz-utils + 运行库；Camoufox 仍用现有 GTK/NSS 依赖
RUN apt-get update && apt-get install -y \
    xvfb \
    x11vnc \
    xz-utils \
    libasound2 \
    libatk-bridge2.0-0 \
    libatk1.0-0 \
    libgtk-3-0 \
    libnss3 \
    libnspr4 \
    libx11-xcb1 \
    libxss1 \
    libxtst6 \
    libgbm1 \
    libdbus-glib-1-2 \
    libxkbcommon0 \
    libxcomposite1 \
    libxdamage1 \
    libxrandr2 \
    libxfixes3 \
    libxext6 \
    libpango-1.0-0 \
    libcairo2 \
    libcups2 \
    libdrm2 \
    libexpat1 \
    python3 \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# 容器内若启用 Clearcote，请在 browser.clearcote.args 显式加 --no-sandbox（项目不会偷偷添加）

# 2. 复制依赖文件、脚本和补丁目录，然后安装
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY scripts/ ./scripts/
COPY patches/ ./patches/
RUN npm install -g pnpm && pnpm install --frozen-lockfile

# 3. 复制源码并初始化
COPY . .
RUN npm run init

EXPOSE 3000 5900

# 4. 启动服务（配置文件会自动从 config.example.yaml 复制到 data/config.yaml）
CMD ["npm", "start", "--", "-xvfb", "-vnc"]
