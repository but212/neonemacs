#!/bin/sh
set -e

echo "==> Building neonemacs for x86_64-unknown-linux-musl..."
cargo build --target x86_64-unknown-linux-musl --release

TARGET_BIN="target/x86_64-unknown-linux-musl/release/neonemacs"
if command -v strip >/dev/null 2>&1; then
    echo "==> Stripping binary..."
    strip "$TARGET_BIN"
fi

SIZE=$(ls -lh "$TARGET_BIN" | awk '{print $5}')
echo "==> Done: $TARGET_BIN ($SIZE)"
