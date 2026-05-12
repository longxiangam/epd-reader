"""Convert sleep.bmp to raw pixel binary for include_bytes! in flash_sleep.rs"""
from PIL import Image
import sys, os

TARGET_W = 300
TARGET_H = 400
ROW_BYTES = (TARGET_W + 7) // 8  # 38

script_dir = os.path.dirname(os.path.abspath(__file__))
bmp_path = os.path.join(script_dir, '..', 'files', 'sleep.bmp')
out_path = os.path.join(script_dir, '..', 'files', 'sleep_default.bin')

img = Image.open(bmp_path)
img = img.resize((TARGET_W, TARGET_H), Image.LANCZOS)
gray = img.convert('L')

pixels = bytearray(ROW_BYTES * TARGET_H)

for y in range(TARGET_H):
    for x in range(TARGET_W):
        lum = gray.getpixel((x, y))
        if lum < 128:
            row_offset = y * ROW_BYTES
            pixels[row_offset + x // 8] |= 1 << (7 - (x % 8))

with open(out_path, 'wb') as f:
    f.write(pixels)

print(f"Generated {out_path}: {len(pixels)} bytes ({TARGET_W}x{TARGET_H}, {ROW_BYTES} bytes/row)")
