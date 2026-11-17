#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf '%s\n' \
        '用法：bash build-support/linux-deps.sh [--check | --install] [--native-tests] [--toolkit-tests]' \
        '默认仅检查，不安装、不修改系统。' \
        '--install 安装缺失的系统依赖，需要用户在终端确认 sudo。' \
        '--native-tests 同时检查隔离原生测试工具。' \
        '--toolkit-tests 检查 Qt Widgets、XRes、Nemo、Chrome 和 Firefox；不启动应用。' \
        '优先使用项目内 .tools/Qt/6.12.0/gcc_64 与 .tools/sysroot，缺失时才回退系统 Qt5/XRes 开发包。' \
        '可用 OINK_QT_KIT 和 OINK_XRES_SYSROOT 覆盖本地路径。' \
        '本脚本不修改 Rust/Node 版本、设备权限、用户组或桌面设置。'
}

mode=check
native_tests=false
toolkit_tests=false
for arg in "$@"; do
    case "$arg" in
        --check) mode=check ;;
        --install) mode=install ;;
        --native-tests) native_tests=true ;;
        --toolkit-tests) toolkit_tests=true ;;
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

script_dir=$(cd -- "${BASH_SOURCE[0]%/*}" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
qt_kit=${OINK_QT_KIT:-"$repo_root/.tools/Qt/6.12.0/gcc_64"}
xres_sysroot=${OINK_XRES_SYSROOT:-"$repo_root/.tools/sysroot"}
failed=0
qt_kit_state=absent
qt_version=
xres_state=absent
xres_version=

if "$toolkit_tests"; then
    if [[ -e "$qt_kit" ]]; then
        if [[ ! -d "$qt_kit" ]]; then
            printf '本地 Qt kit 路径不是目录：%s\n' "$qt_kit" >&2
            qt_kit_state=invalid
            failed=1
        else
            missing_qt=()
            [[ -x "$qt_kit/bin/qmake" ]] || missing_qt+=('bin/qmake')
            [[ -r "$qt_kit/lib/cmake/Qt6Widgets/Qt6WidgetsConfig.cmake" ]] \
                || missing_qt+=('lib/cmake/Qt6Widgets/Qt6WidgetsConfig.cmake')
            [[ -r "$qt_kit/plugins/platforms/libqxcb.so" ]] \
                || missing_qt+=('plugins/platforms/libqxcb.so')
            if ((${#missing_qt[@]})); then
                printf '本地 Qt kit 不完整（%s）：缺少 %s\n' "$qt_kit" "${missing_qt[*]}" >&2
                qt_kit_state=invalid
                failed=1
            else
                raw_version=$("$qt_kit/bin/qmake" -query QT_VERSION 2>/dev/null || true)
                qt_version=${raw_version#QT_VERSION:}
                if [[ "$qt_version" == 6.* ]]; then
                    qt_kit_state=ok
                else
                    printf '本地 Qt kit 无法使用：qmake 未返回 Qt6 版本（%s）\n' "$qt_kit" >&2
                    qt_kit_state=invalid
                    failed=1
                fi
            fi
        fi
    fi

    if [[ -e "$xres_sysroot" ]]; then
        if [[ ! -d "$xres_sysroot" ]]; then
            printf '本地 XRes sysroot 路径不是目录：%s\n' "$xres_sysroot" >&2
            xres_state=invalid
            failed=1
        else
            xres_header="$xres_sysroot/usr/include/X11/extensions/XRes.h"
            xres_pc="$xres_sysroot/usr/lib/x86_64-linux-gnu/pkgconfig/xres.pc"
            xres_lib="$xres_sysroot/usr/lib/x86_64-linux-gnu/libXRes.so"
            xres_lib_versioned="$xres_lib.1.0.0"
            missing_xres=()
            [[ -r "$xres_header" ]] || missing_xres+=("${xres_header#"$xres_sysroot"/}")
            [[ -r "$xres_pc" ]] || missing_xres+=("${xres_pc#"$xres_sysroot"/}")
            [[ -e "$xres_lib" || -e "$xres_lib_versioned" ]] \
                || missing_xres+=("${xres_lib#"$xres_sysroot"/}")
            if ((${#missing_xres[@]})); then
                printf '本地 XRes sysroot 不完整（%s）：缺少 %s\n' \
                    "$xres_sysroot" "${missing_xres[*]}" >&2
                xres_state=invalid
                failed=1
            else
                while IFS= read -r line || [[ -n "$line" ]]; do
                    if [[ "$line" == Version:* ]]; then
                        xres_version=${line#Version:}
                        xres_version=${xres_version#"${xres_version%%[![:space:]]*}"}
                        break
                    fi
                done < "$xres_pc"
                xres_state=ok
            fi
        fi
    fi
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
if "$toolkit_tests"; then
    packages+=(
        libxcb1 libx11-xcb1 libxcb-cursor0 libxcb-icccm4 libxcb-image0
        libxcb-keysyms1 libxcb-randr0 libxcb-render0 libxcb-render-util0
        libxcb-shape0 libxcb-shm0 libxcb-sync1 libxcb-util1 libxcb-xfixes0
        libxcb-xkb1 libxkbcommon-x11-0 libgl1 libegl1 libglx0
    )
    if [[ "$qt_kit_state" == absent ]]; then
        packages+=(qtbase5-dev)
    fi
    if [[ "$xres_state" == absent ]]; then
        packages+=(libxres-dev)
    fi
    packages+=(libxres1 nemo)
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
        if "$toolkit_tests"; then printf ' --toolkit-tests'; fi
        printf '\n' >&2
        exit 1
    fi
fi

modules=(
    gtk+-3.0 webkit2gtk-4.1 ayatana-appindicator3-0.1 librsvg-2.0
    x11 xi xtst xrandr xfixes xrender xext xkbcommon xkbcommon-x11
    alsa libpulse fuse3 pangocairo fontconfig cairo
)
if "$toolkit_tests"; then
    if [[ "$qt_kit_state" == absent ]]; then
        modules+=(Qt5Widgets)
    fi
    if [[ "$xres_state" == absent ]]; then
        modules+=(xres)
    fi
fi
for module in "${modules[@]}"; do
    if version=$(pkg-config --modversion "$module" 2>/dev/null); then
        printf 'OK %-30s %s\n' "$module" "$version"
    else
        printf '缺少开发模块：%s\n' "$module" >&2
        failed=1
    fi
done
if [[ "$qt_kit_state" == ok ]]; then
    printf 'OK %-30s %s（本地）\n' qt-kit "$qt_version"
fi
if [[ "$xres_state" == ok ]]; then
    printf 'OK %-30s %s（本地）\n' xres-sysroot "$xres_version"
fi

commands=(cc c++ make cmake curl xz python3 dpkg-deb fusermount3 fc-match)
if "$native_tests"; then
    commands+=(Xephyr xauth xdpyinfo xinput metacity dbus-daemon dbus-send pipewire pipewire-pulse
        pactl pacat parec pw-cli pw-cat pw-link pw-dump)
fi
if "$toolkit_tests"; then
    commands+=(nemo google-chrome firefox)
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
