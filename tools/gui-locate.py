#!/usr/bin/env python3
"""从截图里找 egui 按钮矩形（底色 (50,50,50) 的圆角块）。

用法：python tools/gui-locate.py <screenshot.png> [x0 y0 x1 y1]
输出每个按钮的中心坐标（原截图坐标空间），按从左到右、从上到下排序。
只做「手和眼」的定位，不做判定。
"""

from __future__ import annotations

import sys

from PIL import Image

BUTTON = (50, 50, 50)
TOL = 6


def is_button(px: tuple[int, int, int]) -> bool:
    return all(abs(a - b) <= TOL for a, b in zip(px, BUTTON))


def main() -> None:
    image = Image.open(sys.argv[1]).convert("RGB")
    x0 = y0 = 0
    x1, y1 = image.size
    if len(sys.argv) >= 5:
        x0, y0, x1, y1 = (int(v) for v in sys.argv[2:6])
    width, height = x1 - x0, y1 - y0
    # 行扫描：找连续的按钮色横段，再向下扩展成矩形
    boxes: list[tuple[int, int, int, int]] = []
    visited = set()
    pixels = image.load()
    for dy in range(height):
        y = y0 + dy
        x = x0
        while x < x1:
            if (x, y) in visited or not is_button(pixels[x, y]):
                x += 1
                continue
            # 横段起点：向右延伸
            start = x
            while x < x1 and is_button(pixels[x, y]):
                x += 1
            run = x - start
            if run < 24:
                continue
            # 向下扩展找矩形高度
            bottom = y
            while bottom + 1 < y1 and all(
                is_button(pixels[cx, bottom + 1]) for cx in (start, start + run // 2, x - 1)
            ):
                bottom += 1
            box = (start, y, x, bottom + 1)
            if (x - start) <= 260 and (bottom + 1 - y) <= 60:
                center = ((box[0] + box[2]) // 2, (box[1] + box[3]) // 2)
                boxes.append(box + center)
                for cy in range(box[1], box[3]):
                    for cx in range(box[0], box[2]):
                        visited.add((cx, cy))
    boxes.sort(key=lambda b: (b[1] // 20, b[0]))
    for box in boxes:
        print(f"center=({box[4]},{box[5]}) size={box[2] - box[0]}x{box[3] - box[1]}")


if __name__ == "__main__":
    main()
