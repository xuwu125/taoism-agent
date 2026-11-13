#!/usr/bin/env python3
"""符箓台 GUI 驱动（机器辅助验收用，不是真人验收）。

为什么需要它：eframe/egui 是自绘窗口，没有 Win32 控件树，UI Automation 找不到
「按钮」；只能按坐标点击 + 截图肉眼/视觉模型判读。本脚本只做「手和眼」，
不做任何判定——判定与如实标注仍由调用方负责。

用法（每行一条命令，也可一次给多条）：
    python tools/gui-drive.py shot out.png
    python tools/gui-drive.py click 640 480
    python tools/gui-drive.py dblclick 640 480
    python tools/gui-drive.py move 640 480
    python tools/gui-drive.py drag 300 500 300 700
    python tools/gui-drive.py scroll 640 480 -5
    python tools/gui-drive.py type "中文也能打（走 WM_CHAR，不经过输入法）"
    python tools/gui-drive.py key enter | key tab | key esc | key ctrl+a | key ctrl+v
    python tools/gui-drive.py sleep 0.5

约定：
- 坐标是**物理像素**（与截图同尺寸）；多显示器取虚拟桌面左上角为原点。
- `type` 走 KEYEVENTF_UNICODE，**绕过输入法**——它验的是 egui 的文本路径，
  验不了中文 IME（IME 必须真人）。
"""

from __future__ import annotations

import ctypes
import sys
import time
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

# 先声明 DPI 感知：否则本进程看到的是「逻辑像素」（如 2048×1152），而截图是物理
# 像素（2560×1440，125% 缩放），坐标会整体偏小一圈，点击全部落空。
try:
    ctypes.WinDLL("shcore").SetProcessDpiAwareness(2)  # PER_MONITOR_AWARE
except OSError:
    user32.SetProcessDPIAware()

SM_XVIRTUALSCREEN = 76
SM_YVIRTUALSCREEN = 77
SM_CXVIRTUALSCREEN = 78
SM_CYVIRTUALSCREEN = 79

INPUT_MOUSE = 0
INPUT_KEYBOARD = 1
KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_UNICODE = 0x0004
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_WHEEL = 0x0800
MOUSEEVENTF_ABSOLUTE = 0x8000
MOUSEEVENTF_VIRTUALDESK = 0x4000


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wintypes.LONG),
        ("dy", wintypes.LONG),
        ("mouseData", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.POINTER(wintypes.ULONG)),
    ]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.POINTER(wintypes.ULONG)),
    ]


class INPUT(ctypes.Structure):
    class _U(ctypes.Union):
        _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT)]

    _anonymous_ = ("u",)
    _fields_ = [("type", wintypes.DWORD), ("u", _U)]


def screen_rect() -> tuple[int, int, int, int]:
    get = user32.GetSystemMetrics
    return (
        get(SM_XVIRTUALSCREEN),
        get(SM_YVIRTUALSCREEN),
        get(SM_CXVIRTUALSCREEN),
        get(SM_CYVIRTUALSCREEN),
    )


def _send(*inputs: INPUT) -> None:
    array = (INPUT * len(inputs))(*inputs)
    sent = user32.SendInput(len(inputs), array, ctypes.sizeof(INPUT))
    if sent != len(inputs):
        raise OSError(ctypes.get_last_error(), "SendInput 未全部送达")


def _mouse(dx: int, dy: int, data: int, flags: int) -> INPUT:
    item = INPUT(type=INPUT_MOUSE)
    item.mi = MOUSEINPUT(dx, dy, data, flags, 0, None)
    return item


def move(x: int, y: int) -> None:
    x0, y0, w, h = screen_rect()
    nx = int((x - x0) * 65535 / max(w - 1, 1))
    ny = int((y - y0) * 65535 / max(h - 1, 1))
    _send(
        _mouse(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )
    )
    time.sleep(0.05)


def click(x: int, y: int, count: int = 1) -> None:
    move(x, y)
    for index in range(count):
        _send(_mouse(0, 0, 0, MOUSEEVENTF_LEFTDOWN), _mouse(0, 0, 0, MOUSEEVENTF_LEFTUP))
        if index + 1 < count:
            time.sleep(0.06)
    time.sleep(0.12)


def drag(x1: int, y1: int, x2: int, y2: int, steps: int = 12) -> None:
    move(x1, y1)
    _send(_mouse(0, 0, 0, MOUSEEVENTF_LEFTDOWN))
    for step in range(1, steps + 1):
        move(x1 + (x2 - x1) * step // steps, y1 + (y2 - y1) * step // steps)
        time.sleep(0.02)
    _send(_mouse(0, 0, 0, MOUSEEVENTF_LEFTUP))
    time.sleep(0.15)


def scroll(x: int, y: int, notches: int) -> None:
    move(x, y)
    _send(_mouse(0, 0, notches * 120, MOUSEEVENTF_WHEEL))
    time.sleep(0.15)


def type_text(text: str) -> None:
    for char in text:
        code = ord(char)
        # 代理对（emoji 等）拆成两个 UTF-16 单元分别发。
        if code > 0xFFFF:
            units = char.encode("utf-16-le")
            codes = [int.from_bytes(units[i : i + 2], "little") for i in (0, 2)]
        else:
            codes = [code]
        for unit in codes:
            down = INPUT(type=INPUT_KEYBOARD)
            down.ki = KEYBDINPUT(0, unit, KEYEVENTF_UNICODE, 0, None)
            up = INPUT(type=INPUT_KEYBOARD)
            up.ki = KEYBDINPUT(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, 0, None)
            _send(down, up)
        time.sleep(0.01)


VK = {
    "enter": 0x0D,
    "tab": 0x09,
    "esc": 0x1B,
    "space": 0x20,
    "back": 0x08,
    "delete": 0x2E,
    "insert": 0x2D,
    "home": 0x24,
    "end": 0x23,
    "up": 0x26,
    "down": 0x28,
    "left": 0x25,
    "right": 0x27,
    "pageup": 0x21,
    "pagedown": 0x22,
    "f5": 0x74,
}
MOD = {"ctrl": 0x11, "shift": 0x10, "alt": 0x12}


def key(combo: str) -> None:
    parts = combo.lower().split("+")
    name = parts[-1]
    mods = [MOD[p] for p in parts[:-1] if p in MOD]
    code = VK.get(name) or (ord(name.upper()) if len(name) == 1 else None)
    if code is None:
        raise SystemExit(f"不认识的键：{combo}")
    for modifier in mods:
        down = INPUT(type=INPUT_KEYBOARD)
        down.ki = KEYBDINPUT(modifier, 0, 0, 0, None)
        _send(down)
    down = INPUT(type=INPUT_KEYBOARD)
    down.ki = KEYBDINPUT(code, 0, 0, 0, None)
    up = INPUT(type=INPUT_KEYBOARD)
    up.ki = KEYBDINPUT(code, 0, KEYEVENTF_KEYUP, 0, None)
    _send(down, up)
    for modifier in reversed(mods):
        up = INPUT(type=INPUT_KEYBOARD)
        up.ki = KEYBDINPUT(modifier, 0, KEYEVENTF_KEYUP, 0, None)
        _send(up)
    time.sleep(0.12)


def shot(path: str) -> None:
    from PIL import ImageGrab

    image = ImageGrab.grab(all_screens=True)
    image.save(path)
    print(f"saved {path} {image.size[0]}x{image.size[1]}")


def find_window(title: str, focus: bool = False, maximize: bool = False) -> None:
    """按标题找顶层窗口：打印句柄与矩形；`focus` 置前并恢复；`maximize` 最大化。"""
    hwnd = user32.FindWindowW(None, title)
    if not hwnd:
        raise SystemExit(f"找不到窗口：{title}")
    rect = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    print(f"hwnd=0x{hwnd:X} rect=({rect.left},{rect.top})-({rect.right},{rect.bottom})")
    if maximize:
        user32.ShowWindow(hwnd, 3)  # SW_MAXIMIZE
        time.sleep(0.4)
    if focus:
        # SW_RESTORE 先解开最小化，再置前；两步都做，哪个生效算哪个。
        user32.ShowWindow(hwnd, 9)
        user32.SetForegroundWindow(hwnd)
        time.sleep(0.4)
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        print(f"focused rect=({rect.left},{rect.top})-({rect.right},{rect.bottom})")


def move_window(title: str, x: int, y: int) -> None:
    """按标题移动顶层窗口到 (x, y)（不改尺寸），并置为「总在最前」（TOPMOST）。"""
    hwnd = user32.FindWindowW(None, title)
    if not hwnd:
        raise SystemExit(f"找不到窗口：{title}")
    # HWND_TOPMOST=-1：浏览器等其它窗口盖不住它；SWP_NOSIZE=0x0001 只移动。
    user32.SetWindowPos(hwnd, -1, x, y, 0, 0, 0x0001)
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.3)
    rect = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    print(f"moved rect=({rect.left},{rect.top})-({rect.right},{rect.bottom})")


def run(argv: list[str]) -> None:
    if not argv:
        raise SystemExit(__doc__)
    index = 0
    while index < len(argv):
        command = argv[index]
        index += 1
        if command == "shot":
            shot(argv[index])
            index += 1
        elif command == "click":
            click(int(argv[index]), int(argv[index + 1]))
            index += 2
        elif command == "dblclick":
            click(int(argv[index]), int(argv[index + 1]), 2)
            index += 2
        elif command == "move":
            move(int(argv[index]), int(argv[index + 1]))
            index += 2
        elif command == "drag":
            drag(
                int(argv[index]),
                int(argv[index + 1]),
                int(argv[index + 2]),
                int(argv[index + 3]),
            )
            index += 4
        elif command == "scroll":
            scroll(int(argv[index]), int(argv[index + 1]), int(argv[index + 2]))
            index += 3
        elif command == "type":
            type_text(argv[index])
            index += 1
        elif command == "key":
            key(argv[index])
            index += 1
        elif command == "sleep":
            time.sleep(float(argv[index]))
            index += 1
        elif command == "find":
            find_window(argv[index], focus=False)
            index += 1
        elif command == "focus":
            find_window(argv[index], focus=True)
            index += 1
        elif command == "max":
            find_window(argv[index], focus=True, maximize=True)
            index += 1
        elif command == "movewin":
            move_window(argv[index], int(argv[index + 1]), int(argv[index + 2]))
            index += 3
        elif command == "screen":
            print(screen_rect())
        else:
            raise SystemExit(f"不认识的命令：{command}")


if __name__ == "__main__":
    run(sys.argv[1:])
