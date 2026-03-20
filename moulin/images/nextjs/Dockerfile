# Next.js sandbox environment
FROM node:22-slim

# The build runner currently propagates TMPDIR=/var/run/moulin_home/tmp which doesn't exist yet
# Create it manually so apt-get/dpkg doesn't fail on mktemp
RUN mkdir -p /var/run/moulin_home/tmp && chmod 777 /var/run/moulin_home/tmp

# Install procps, curl, and wget
RUN apt-get update && apt-get install -y procps curl wget && rm -rf /var/lib/apt/lists/*

# Set npm to use Chinese mirror
RUN npm config set registry https://registry.npmmirror.com

# Create workspace directory
RUN mkdir -p /home/moulin/workspace
WORKDIR /home/moulin/workspace

# Pre-install create-next-app globally so scaffolding is fast inside the sandbox
RUN npm install -g create-next-app

ENTRYPOINT ["sleep", "infinity"]
