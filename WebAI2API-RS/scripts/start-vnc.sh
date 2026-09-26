#!/bin/sh
# 启动 x11vnc，只监听本地。参数：显示号、端口。由 webai2api 在 -vnc 模式下调用。
if ! command -v x11vnc >/dev/null 2>&1; then
    echo "未找到 x11vnc 命令" >&2
    exit 1
fi
DISPLAY_NUM="$1"
PORT="$2"
case "$DISPLAY_NUM" in
    :[0-9]*) ;;
    *) echo "非法显示号: $DISPLAY_NUM" >&2; exit 1 ;;
esac
case "$PORT" in
    59[0-9][0-9]) ;;
    *) echo "非法端口: $PORT" >&2; exit 1 ;;
esac
exec x11vnc -display "$DISPLAY_NUM" -rfbport "$PORT" -localhost -nopw \
    -shared -forever -noxdamage -norc -geometry 1366x768
