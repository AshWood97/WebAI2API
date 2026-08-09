FROM foxhui/webai-2api:latest

USER root

WORKDIR /app

ENV DEBIAN_FRONTEND=noninteractive
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=true

# 1. 安装系统依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    tigervnc-scraping-server \
    && rm -rf /var/lib/apt/lists/*

# 2. 复制依赖文件、脚本和补丁目录，然后安装
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY scripts/postinstall.js ./scripts/postinstall.js
COPY patches/ ./patches/
RUN npm install -g pnpm && CI=true pnpm install --force --frozen-lockfile

# 3. 复制源码。基础镜像已包含与其发布版本匹配的 Camoufox；不要在低内存
# 服务器上重复下载浏览器二进制，依赖安装已完成 native module 自检。
COPY . .

# X0tigervnc remains foreground-supervised by the upstream supervisor.  The
# original x11vnc implementation in the upstream image can expose a TCP port
# without completing the RFB handshake in this deployment.
COPY scripts/start-with-tigervnc.sh /usr/local/bin/start-with-tigervnc
RUN chmod 0755 /usr/local/bin/start-with-tigervnc

EXPOSE 3000 5900

# 4. 启动服务（配置文件会自动从 config.example.yaml 复制到 data/config.yaml）
ENTRYPOINT ["/usr/local/bin/start-with-tigervnc"]
CMD ["npm", "start", "--", "-xvfb", "-vnc"]
