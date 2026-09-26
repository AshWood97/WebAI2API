#!/bin/sh
# 启动 Xvfb 虚拟显示。参数：显示号（如 :51）。由 webai2api 在 -xvfb 模式下调用。
# 屏幕参数固定，不接受其他输入。
if ! command -v Xvfb >/dev/null 2>&1; then
    echo "未找到 Xvfb。请先安装: Ubuntu/Debian: sudo apt install xvfb" >&2
    exit 1
fi
DISPLAY_NUM="$1"
case "$DISPLAY_NUM" in
    :[0-9]*) ;;
    *) echo "非法显示号: $DISPLAY_NUM" >&2; exit 1 ;;
esac
exec Xvfb "$DISPLAY_NUM" -ac -screen 0 1366x768x24
