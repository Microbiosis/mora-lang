# Mora v0.104.5 — AI 原生脚本语言
# 多阶段构建：编译阶段 + 运行阶段
#
# 修复历史：v0.51 在 Cargo.toml 引入 `build = "build.rs"`，本 Dockerfile 在 v0.25 之后
# 一直未同步。v0.51 ~ v0.104.x 期间无 tag push 触发 Docker job，该 bug 潜伏。
# v0.104.5 推送 tag 后第一次撞上，报 `couldn't read 'build.rs'` 失败。
# 故 build.rs 必须显式 COPY 到 build context 根。

# ============================================================
# Stage 1: 编译
# ============================================================
FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

# 一次性 COPY 顶层 manifest + build script + 源代码树。
# Cargo.toml / Cargo.lock / build.rs 改动频率低于 src/，独立 COPY 能让 Docker 层缓存
# 在改 src/ 时复用 manifest 层。
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ src/
COPY examples/ examples/

# 构建 mora + mora-lsp (musl 静态链接，避免 glibc 兼容问题)
RUN cargo build --release --target x86_64-unknown-linux-musl && \
    strip target/x86_64-unknown-linux-musl/release/mora && \
    strip target/x86_64-unknown-linux-musl/release/mora-lsp

# ============================================================
# Stage 2: 运行时
# ============================================================
FROM alpine:3.21

RUN apk add --no-cache ca-certificates

# 非 root 用户
RUN adduser -D -s /bin/sh mora
USER mora
WORKDIR /home/mora

# 编译产物
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mora     /usr/local/bin/
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mora-lsp /usr/local/bin/

# 示例脚本 (mora 用户可读)
COPY --chown=mora:mora examples/ /home/mora/examples/

# 默认启动 REPL
CMD ["mora", "--repl"]

