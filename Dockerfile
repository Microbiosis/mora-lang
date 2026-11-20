# Mora v0.110.0 — AI-native 静态类型语言
# 多阶段构建：编译阶段 + 运行阶段
#
# 修复历史：v0.51 在 Cargo.toml 引入 `build = "build.rs"`，v0.110.0 引入 `std/` 工作区。
# 故 build.rs 与 std/ 必须显式 COPY 到 build context 根。

# ============================================================
# Stage 1: 编译
# ============================================================
FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

# 一次性 COPY 顶层 manifest + build script + 标准库工作区 + 源代码树。
COPY Cargo.toml Cargo.lock build.rs ./
COPY std/ std/
COPY src/ src/
COPY examples/ examples/

# 构建 mora + mora-lsp (musl 静态链接，避免 glibc 兼容问题)
RUN cargo build --release --target x86_64-unknown-linux-musl --bin mora --bin mora-lsp && \
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

# 示例脚本与标准库 (mora 用户可读)
COPY --chown=mora:mora examples/ /home/mora/examples/
COPY --chown=mora:mora std/      /home/mora/std/

# 默认启动 REPL
CMD ["mora", "--repl"]
