# 使用 node 基础镜像替代 sandbox，避免携带庞大依赖
FROM node:20-slim

# 2. 安装 Chromium 及其最小依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    chromium \
    fonts-liberation \
    libnss3 \
    libatk-bridge2.0-0 \
    libdrm2 \
    libxkbcommon0 \
    libgbm1 \
    libasound2 \
    && rm -rf /var/lib/apt/lists/*

ENV PUPPETEER_SKIP_CHROMIUM_DOWNLOAD=true \
    PUPPETEER_EXECUTABLE_PATH=/usr/lib/chromium/chromium \
    NODE_PATH=/usr/local/lib/node_modules

# 3. 创建工作目录
RUN mkdir -p /home/moulin/workspace

# 5. 设置工作目录
WORKDIR /home/moulin/workspace

# 6. 以 root 身份安装 Puppeteer（避免权限问题）
RUN npm init -y
RUN npm install puppeteer --ignore-scripts

ENTRYPOINT ["sleep", "infinity"]