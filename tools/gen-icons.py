#!/usr/bin/env python3
"""从几何定义生成 AOProxy 的位图图标。

assets/icons/aoproxy.svg 是给人看的矢量源；本脚本用同一组几何常量
（GEOM）把图标光栅化为 PNG 与 ICO。之所以不直接渲染 SVG，是为了不引入
任何第三方依赖——只用 Python 标准库，任何机器上都能重跑。

    python tools/gen-icons.py

产物（全部写入 assets/icons/）：
    aoproxy-{16,32,48,64,128,256}.png   Linux 图标
    aoproxy.ico                         Windows exe 与窗口图标
    tray-active.ico / tray-idle.ico     托盘两态（内含 16/24/32）
"""

import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                   "assets", "icons")

# 全部取值都是边长的比例，缩放到任何尺寸都一致。
GEOM = {
    "margin": 0.055,    # 圆角方形到画布边缘的留白
    "radius": 0.215,    # 圆角半径
    "shaft_h": 0.105,   # 箭杆粗细
    "head_w": 0.150,    # 箭头三角形的长度
    "head_h": 0.225,    # 箭头三角形的高度
    "gap": 0.085,       # 两支箭之间的间距
    "inset": 0.185,     # 箭组到方形内侧的留白
    "stroke": 0.070,    # 描边态的线宽
}

GRAD_FROM = (0x2F, 0xB8, 0xC6)   # 青
GRAD_TO = (0x3B, 0x6F, 0xE0)     # 蓝
ARROW = (0xFF, 0xFF, 0xFF)
IDLE = (0x8A, 0x8F, 0x98)        # 停止态的灰

SS = 4  # 每轴超采样倍数，用于抗锯齿


def rounded_rect(x, y, lo, hi, r):
    """点是否落在 [lo,hi] 的圆角方形内。"""
    cx = min(max(x, lo + r), hi - r)
    cy = min(max(y, lo + r), hi - r)
    if x == cx and y == cy:
        return True
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_arrow(x, y, cy, points_right, g):
    """点是否落在一支箭内（箭杆矩形 ∪ 箭头三角形）。"""
    lo = g["margin"] + g["inset"]
    hi = 1.0 - g["margin"] - g["inset"]
    half = g["shaft_h"] / 2.0
    if points_right:
        tip, base = hi, hi - g["head_w"]
        if lo <= x <= base and abs(y - cy) <= half:
            return True
        if base <= x <= tip:
            t = (tip - x) / g["head_w"]
            return abs(y - cy) <= (g["head_h"] / 2.0) * t
    else:
        tip, base = lo, lo + g["head_w"]
        if base <= x <= hi and abs(y - cy) <= half:
            return True
        if tip <= x <= base:
            t = (x - tip) / g["head_w"]
            return abs(y - cy) <= (g["head_h"] / 2.0) * t
    return False


def in_arrows(x, y, g):
    offset = (g["shaft_h"] + g["gap"]) / 2.0
    return (in_arrow(x, y, 0.5 - offset, True, g)
            or in_arrow(x, y, 0.5 + offset, False, g))


def render(size, style):
    """光栅化为 RGBA 字节串。style 为 "solid"（填充）或 "outline"（描边）。"""
    g = GEOM
    lo, hi = g["margin"], 1.0 - g["margin"]
    r = g["radius"]
    inner = g["stroke"]
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(SS):
                for sx in range(SS):
                    x = (px + (sx + 0.5) / SS) / size
                    y = (py + (sy + 0.5) / SS) / size
                    if not rounded_rect(x, y, lo, hi, r):
                        continue
                    if style == "solid":
                        # 沿对角线的线性渐变
                        t = max(0.0, min(1.0, (x + y - 2 * lo) / (2 * (hi - lo))))
                        col = tuple(GRAD_FROM[i] + (GRAD_TO[i] - GRAD_FROM[i]) * t
                                    for i in range(3))
                        if in_arrows(x, y, g):
                            col = ARROW
                    else:
                        # 描边态：只画边框环与箭头，中间镂空
                        ring = not rounded_rect(x, y, lo + inner, hi - inner,
                                                max(0.0, r - inner))
                        if not (ring or in_arrows(x, y, g)):
                            continue
                        col = IDLE
                    for i in range(3):
                        acc[i] += col[i]
                    acc[3] += 255.0
            n = SS * SS
            a = acc[3] / n
            if a <= 0.5:
                row += b"\x00\x00\x00\x00"
            else:
                # 颜色按已覆盖的子样本求均值，避免边缘发暗
                w = acc[3] / 255.0
                row += bytes((int(round(acc[i] / w)) for i in range(3))) \
                    + bytes((int(round(a)),))
        rows.append(bytes(row))
    return rows


def png(rows, size):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body \
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + r for r in rows)
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


def ico(entries):
    """把若干 PNG 打包成 ICO。Vista 起的 Windows 与 Qt 都支持 PNG 内嵌。"""
    head = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    dirs, blobs = b"", b""
    for size, data in entries:
        dim = 0 if size >= 256 else size
        dirs += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    return head + dirs + blobs


def main():
    os.makedirs(OUT, exist_ok=True)
    made = []

    app = {}
    for size in (16, 32, 48, 64, 128, 256):
        app[size] = png(render(size, "solid"), size)
        path = os.path.join(OUT, "aoproxy-%d.png" % size)
        with open(path, "wb") as fh:
            fh.write(app[size])
        made.append(path)

    for name, sizes in (("aoproxy.ico", (16, 32, 48, 256)),):
        path = os.path.join(OUT, name)
        with open(path, "wb") as fh:
            fh.write(ico([(s, app[s]) for s in sizes]))
        made.append(path)

    for name, style in (("tray-active.ico", "solid"), ("tray-idle.ico", "outline")):
        entries = [(s, png(render(s, style), s)) for s in (16, 24, 32)]
        path = os.path.join(OUT, name)
        with open(path, "wb") as fh:
            fh.write(ico(entries))
        made.append(path)

    for path in made:
        print("%8d  %s" % (os.path.getsize(path), os.path.relpath(path)))


if __name__ == "__main__":
    main()
