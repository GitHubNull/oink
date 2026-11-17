#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf '%s\n' \
        '用法：bash build-support/linux-deps.sh [--check | --install] [--native-tests]' \
        '默认仅检查，不安装、不修改系统。' \
        '--install 安装缺失的系统依赖，需要用户在终端确认 sudo。' \
        '--native-tests 同时检查隔离原生测试工具。' \
        '本脚本不修改 Rust/Node 版本、设备权限、用户组或桌面设置。'
}

mode=check
native_tests=false
for arg in "$@"; do
    case "$arg" in
        --check) mode=check ;;
        --install) mode=install ;;
        --native-tests) native_tests=true ;;
        -h|--help) usage; exit 0 ;;
        *) printf '未知参数：%s\n' "$arg" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ $(uname -s) != Linux ]]; then
    printf '此脚本仅适用于 Linux。\n' >&2
    exit 2
fi
if [[ ! -r /etc/os-release ]]; then
    printf '无法识别发行版：缺少 /etc/os-release。\n' >&2
    exit 2
fi
. /etc/os-release
case " ${ID:-} ${ID_LIKE:-} " in
    *' ubuntu '*|*' debian '*|*' linuxmint '*) ;;
    *) printf '当前仅支持 Debian/Ubuntu/Linux Mint，检测到：%s\n' "${ID:-unknown}" >&2; exit 2 ;;
esac
if [[ $(uname -m) != x86_64 ]]; then
    printf '本轮 Linux 构建基线为 x86_64，当前架构：%s\n' "$(uname -m)" >&2
    exit 2
fi

packages=(
    build-essential pkg-config cmake curl ca-certificates xz-utils
    python3 dpkg-dev
    libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev
    libx11-dev libxi-dev libxtst-dev libxrandr-dev libxfixes-dev libxrender-dev libxext-dev
    libxkbcommon-dev libxkbcommon-x11-dev
    libasound2-dev libpulse-dev libfuse3-dev fuse3
    libpango1.0-dev libfontconfig1-dev libcairo2-dev fonts-noto-cjk
)
if "$native_tests"; then
    packages+=(xserver-xephyr xauth x11-utils xinput metacity dbus-daemon pipewire pipewire-bin pipewire-pulse pulseaudio-utils)
fi

missing=()
for package in "${packages[@]}"; do
    status=$(dpkg-query -W -f='${db:Status-Status}' "$package" 2>/dev/null || true)
    if [[ "$status" != installed ]]; then
        missing+=("$package")
    fi
done

if ((${#missing[@]})); then
    printf '缺少系统包：\n'
    printf '  %s\n' "${missing[@]}"
    if [[ "$mode" == install ]]; then
        printf '仅安装以上缺失包；不会修改设备权限、音频服务配置或自启动项。\n'
        if ((EUID == 0)); then
            apt-get update
            apt-get install --no-install-recommends "${missing[@]}"
        else
            sudo apt-get update
            sudo apt-get install --no-install-recommends "${missing[@]}"
        fi
    else
        printf '请由用户执行：bash build-support/linux-deps.sh --install'
        if "$native_tests"; then printf ' --native-tests'; fi
        printf '\n' >&2
        exit 1
    fi
fi

modules=(
    gtk+-3.0 webkit2gtk-4.1 ayatana-appindicator3-0.1 librsvg-2.0
    x11 xi xtst xrandr xfixes xrender xext xkbcommon xkbcommon-x11
    alsa libpulse fuse3 pangocairo fontconfig cairo
)
failed=0
for module in "${modules[@]}"; do
    if version=$(pkg-config --modversion "$module" 2>/dev/null); then
        printf 'OK %-30s %s\n' "$module" "$version"
    else
        printf '缺少开发模块：%s\n' "$module" >&2
        failed=1
    fi
done

commands=(cc c++ make cmake curl xz python3 dpkg-deb fusermount3 fc-match)
if "$native_tests"; then
    commands+=(Xephyr xauth xdpyinfo xinput metacity dbus-daemon dbus-send pipewire pipewire-pulse
        pactl pacat parec pw-cli pw-cat pw-link pw-dump)
fi
for command in "${commands[@]}"; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf '缺少命令：%s\n' "$command" >&2
        failed=1
    fi
done
if [[ ! -c /dev/fuse || ! -r /dev/fuse || ! -w /dev/fuse ]]; then
    printf '当前用户无法读写 /dev/fuse；请检查系统配置，本脚本不会放宽设备权限。\n' >&2
    failed=1
fi
if ((failed)); then
    exit 1
fi
printf 'Linux 系统依赖检查通过；工具链版本、隔离会话与功能验收需另行检查。\n'
