"""
从 QWeather Icons (github.com/qwd/Icons) 下载 SVG 天气图标，
使用 resvg CLI 转换为 PNG，再用 Pillow 转为 32x32 单色 BMP。

依赖: Pillow
外部工具: resvg CLI (已下载到 /tmp/resvg_cli/resvg.exe)
"""

import os
import subprocess
import urllib.request
import tempfile
from pathlib import Path
from PIL import Image

SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_DIR = SCRIPT_DIR.parent
OUTPUT_DIR = PROJECT_DIR / "icons" / "weather"

ICON_SIZE = 32
RENDER_SIZE = 256

BASE_URL = "https://raw.githubusercontent.com/qwd/Icons/main/icons/"
RESVG = SCRIPT_DIR / "resvg.exe"

# WeatherKind 名称 → QWeather 图标编号
ICON_MAP = {
    "sunny":          "100",
    "partly_cloudy":  "101",
    "mostly_cloudy":  "103",
    "cloudy":         "102",
    "overcast":       "104",
    "light_rain":     "305",
    "moderate_rain":  "306",
    "heavy_rain":     "312",
    "thunderstorm":   "302",
    "sleet":          "404",
    "light_snow":     "400",
    "moderate_snow":  "401",
    "heavy_snow":     "403",
    "dust":           "504",
    "fog":            "501",
    "haze":           "502",
    "wind":           "503",
    "cold":           "901",
    "hot":            "900",
    "unknown":        "999",
}


def download_svg(code: str, dest: Path) -> bool:
    url = f"{BASE_URL}{code}.svg"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
        with urllib.request.urlopen(req, timeout=15) as resp:
            if resp.status == 200:
                dest.write_bytes(resp.read())
                print(f"  下载 {code}.svg OK")
                return True
    except Exception:
        return False


def svg_to_png(svg_path: Path, png_path: Path):
    cmd = [str(RESVG), "-w", str(RENDER_SIZE), "-h", str(RENDER_SIZE), str(svg_path), str(png_path)]
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"resvg 失败: {result.stderr}")


def png_to_bmp(png_path: Path) -> Image.Image:
    img = Image.open(png_path).convert("RGBA")
    # 透明 → 白色背景
    bg = Image.new("RGBA", img.size, (255, 255, 255, 255))
    bg.paste(img, mask=img.split()[3])
    img = bg.convert("L")
    # 缩放到 32x32
    img = img.resize((ICON_SIZE, ICON_SIZE), Image.LANCZOS)
    # 二值化 + 反色（e-ink: bit1=BinaryColor::On=黑=绘制, bit0=白=不绘制）
    # 深色像素 → 255(bit1=绘制为黑), 浅色像素 → 0(bit0=不绘制=白)
    img = img.point(lambda p: 255 if p < 128 else 0, "1")
    return img


def main():
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    if not RESVG.exists():
        print(f"错误: resvg 不存在 ({RESVG})")
        return

    print(f"下载并转换天气图标 ({ICON_SIZE}x{ICON_SIZE})")
    print(f"  来源: QWeather Icons")
    print(f"  工具: {RESVG}")
    print()

    with tempfile.TemporaryDirectory() as tmpdir:
        tmpdir = Path(tmpdir)
        success = 0
        failed = []

        for name, code in ICON_MAP.items():
            print(f"[{name}] (code: {code})")
            svg_path = tmpdir / f"{code}.svg"
            png_path = tmpdir / f"{code}.png"

            try:
                if not download_svg(code, svg_path):
                    raise RuntimeError("下载失败")

                svg_to_png(svg_path, png_path)
                img = png_to_bmp(png_path)

                out_path = OUTPUT_DIR / f"{name}.bmp"
                img.save(str(out_path), "BMP")
                print(f"  保存 {out_path.name}")
                success += 1
            except Exception as e:
                print(f"  FAILED: {e}")
                failed.append(name)

    print(f"\n完成: {success}/{len(ICON_MAP)}")
    if failed:
        print(f"失败: {', '.join(failed)}")


if __name__ == "__main__":
    main()
