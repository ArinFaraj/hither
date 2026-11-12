#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
NDK=$(ls -d "$HOME"/Library/Android/sdk/ndk/* | sort -V | tail -1)
TC="$NDK/toolchains/llvm/prebuilt/darwin-x86_64"
export CC_aarch64_unknown_linux_musl="$TC/bin/clang"
export CFLAGS_aarch64_unknown_linux_musl="--target=aarch64-linux-musl -isystem $TC/sysroot/usr/include -isystem $TC/sysroot/usr/include/aarch64-linux-android"
export AR_aarch64_unknown_linux_musl="$TC/bin/llvm-ar"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
cargo build --release --target aarch64-unknown-linux-musl
echo target/aarch64-unknown-linux-musl/release/hither
