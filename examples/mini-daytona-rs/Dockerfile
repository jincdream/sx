# syntax=docker/dockerfile:1.7
FROM rust:latest AS builder

# Use Tsinghua mirror for faster downloads (works on both amd64 and arm64)
RUN if [ -f /etc/apt/sources.list.d/debian.sources ]; then \
        sed -i 's/deb.debian.org/mirrors.tuna.tsinghua.edu.cn/g' /etc/apt/sources.list.d/debian.sources; \
    elif [ -f /etc/apt/sources.list ]; then \
        sed -i 's/deb.debian.org/mirrors.tuna.tsinghua.edu.cn/g' /etc/apt/sources.list; \
    fi

WORKDIR /work

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libseccomp-dev \
    iproute2 \
    iptables

# Step 1: Cache dependencies — only re-runs when Cargo.toml changes
COPY .cargo/config.toml .cargo/config.toml
COPY Cargo.toml Cargo.lock* ./
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    mkdir src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/mini-daytona-rs target/release/deps/mini_daytona_rs-* target/release/.fingerprint/mini-daytona-rs-*

# Step 2: Build actual source — uses cached deps from above
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --release

FROM scratch AS artifact
COPY --from=builder /work/target/release/mini-daytona-rs /mini-daytona-rs

FROM builder AS runtime

# Setup a test directory with Nginx dummy Dockerfile for integration testing
# RUN mkdir -p /work/tests/e2e && \
#     echo "FROM nginx:alpine\nRUN echo 'Hello Mini Daytona' > /usr/share/nginx/html/index.html" > /work/tests/e2e/Dockerfile.nginx && \
#     echo "FROM python:3.9-slim\nRUN echo 'Python works' > /opt/test.txt" > /work/tests/e2e/Dockerfile.python

EXPOSE 3000

CMD ["./target/release/mini-daytona-rs", "server"]
