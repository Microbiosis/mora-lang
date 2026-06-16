# Mora v0.04 — AI 原生 + 云服务原生（单二进制多协议）
# 多阶段构建：编译阶段 + 运行阶段

# ============================================================
# Stage 1: 编译
# ============================================================
FROM rust:1.86-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY examples/ examples/

# v0.04 终态：只 build 2 个二进制（mora + mora-lsp）
# mora-mcp / mora-serve 已删除（v0.04 终态用单二进制多协议）
RUN cargo build --release --target x86_64-unknown-linux-musl && \
    strip target/x86_64-unknown-linux-musl/release/mora && \
    strip target/x86_64-unknown-linux-musl/release/mora-lsp

# ============================================================
# Stage 2: 运行时
# ============================================================
FROM alpine:3.21

RUN apk add --no-cache ca-certificates

# 创建非 root 用户
RUN adduser -D -s /bin/sh mora
USER mora
WORKDIR /home/mora

# 复制编译产物
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mora /usr/local/bin/
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mora-lsp /usr/local/bin/

# 复制示例脚本
COPY --chown=mora:mora examples/ /home/mora/examples/

# 默认运行 REPL
CMD ["mora", "--repl"]
