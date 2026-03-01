FROM rust:latest

RUN sed -i 's/deb.debian.org/mirrors.tuna.tsinghua.edu.cn/g' /etc/apt/sources.list.d/debian.sources

WORKDIR /work

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .

RUN cargo build --release

# Setup a test directory with Nginx dummy Dockerfile for integration testing
RUN mkdir -p /work/tests/e2e && \
    echo "FROM nginx:alpine\nRUN echo 'Hello Mini Daytona' > /usr/share/nginx/html/index.html" > /work/tests/e2e/Dockerfile.nginx && \
    echo "FROM python:3.9-slim\nRUN echo 'Python works' > /opt/test.txt" > /work/tests/e2e/Dockerfile.python

EXPOSE 3000

CMD ["./target/release/mini-daytona-rs", "server"]
