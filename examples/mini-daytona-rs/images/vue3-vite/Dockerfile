# Vue3 Vite sandbox environment
FROM node:20-slim

# Create workspace directory
RUN mkdir -p /home/daytona/workspace
WORKDIR /home/daytona/workspace

# Pre-install create-vite globally so scaffolding is fast inside the sandbox
RUN npm install -g create-vite

ENTRYPOINT ["sleep", "infinity"]
