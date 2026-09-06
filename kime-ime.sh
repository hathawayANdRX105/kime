#!/bin/bash
# kime-ime — 切换 fcitx5 ↔ kime
# 用法：kime-ime [kime|fcitx5|toggle|status|restart]
set -euo pipefail

KIME_BIN="${KIME_BIN:-$HOME/projects/kime/target/release/platform-wayland}"
DEBUG_BIN="${HOME}/projects/kime/target/debug/platform-wayland"
LOG_FILE="${KIME_LOG:-/tmp/kime-ime.log}"

is_kime_running() { pgrep -f 'platform-wayland' >/dev/null 2>&1; }
is_fcitx5_running() { pgrep -x fcitx5 >/dev/null 2>&1; }

pick_kime_bin() {
    [[ -x "$KIME_BIN" ]] && { echo "$KIME_BIN"; return; }
    [[ -x "$DEBUG_BIN" ]] && { echo "$DEBUG_BIN"; return; }
    echo "错误：找不到 kime 二进制，先编译：cargo build -p platform-wayland --release" >&2
    exit 1
}

start_kime() {
    if is_kime_running; then echo "kime 已在运行"; return; fi
    local bin; bin="$(pick_kime_bin)"
    setsid "$bin" >>"$LOG_FILE" 2>&1 &
    disown $! 2>/dev/null || true
    for _ in $(seq 1 30); do
        is_kime_running && { echo "✓ kime 已启动 → $bin"; return; }
        sleep 0.1
    done
    echo "✗ kime 启动失败，看日志：tail $LOG_FILE" >&2
    return 1
}

stop_kime() {
    if is_kime_running; then
        pkill -f 'platform-wayland'
        for _ in $(seq 1 30); do
            is_kime_running || { echo "✓ kime 已停止"; return; }
            sleep 0.1
        done
        pkill -9 -f 'platform-wayland' 2>/dev/null || true
        echo "✓ kime 已强制停止"
    fi
}

start_fcitx5() {
    if is_fcitx5_running; then echo "fcitx5 已在运行"; return; fi
    setsid fcitx5 -d </dev/null >/dev/null 2>&1 &
    disown $! 2>/dev/null || true
    echo "✓ fcitx5 已启动"
}

stop_fcitx5() {
    if is_fcitx5_running; then
        pkill -x fcitx5
        for _ in $(seq 1 20); do
            is_fcitx5_running || { echo "✓ fcitx5 已停止"; return; }
            sleep 0.1
        done
        pkill -9 -x fcitx5 2>/dev/null || true
        echo "✓ fcitx5 已强制停止"
    fi
}

toggle() {
    if is_kime_running; then
        stop_kime; start_fcitx5
    elif is_fcitx5_running; then
        stop_fcitx5; start_kime
    else
        echo "两个都没在跑，默认启动 kime"; start_kime
    fi
}

case "${1:-toggle}" in
    kime)     stop_fcitx5; start_kime ;;
    fcitx5)   stop_kime;   start_fcitx5 ;;
    restart)  stop_kime;   start_kime ;;
    toggle)   toggle ;;
    status)   is_kime_running && echo "当前: kime" || { is_fcitx5_running && echo "当前: fcitx5" || echo "当前: 无"; } ;;
    *)        echo "用法: $0 [kime|fcitx5|toggle|restart|status]" >&2; exit 2 ;;
esac
