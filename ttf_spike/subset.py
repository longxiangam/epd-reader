# -*- coding: utf-8 -*-
"""从 simhei.ttf 子集化出验证用 TTF。
- font.ttf  : 覆盖 sample.txt 的全部字符，供固件从 SD 卡读取后整屏渲染。
              固件用 include_str!("sample.txt") 作为渲染文本，二者字符集完全一致。
- subset.ttf: 小固定集合，供 ttf_spike 内 run() 基准 / draw_ttf_text 使用（嵌入固件）。
"""
import os, sys, io
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")
from fontTools import subset

SIMHEI = r"C:\Windows\Fonts\simhei.ttf"
HERE = os.path.dirname(os.path.abspath(__file__))

COMMON_OPTS = [
    "--no-hinting",            # e-ink 二值化不需要 hinting，减小体积
    "--desubroutinize",
    "--drop-tables+=GSUB,GPOS,vhea,vmtx,hdmx,VDMX,DSIG,cvt,prep,fpgm,gasp,name",
    "--notdef-outline",
    "--recalc-bounds",
    "--recalc-timestamp",
]

def subset_to(text: str, out: str):
    chars = sorted(set(text))
    cjk = sum(1 for c in chars if ord(c) > 0x2000)
    args = [SIMHEI, f"--text={text}", f"--output-file={out}"] + COMMON_OPTS
    subset.main(args)
    sz = os.path.getsize(out)
    print(f"{os.path.basename(out)}: {len(chars)} unique chars ({cjk} CJK), {sz} bytes ({sz/1024:.1f} KB)")

# 1) font.ttf —— 来自 sample.txt（固件渲染文本与此完全一致）
sample = open(os.path.join(HERE, "sample.txt"), encoding="utf-8").read()
subset_to(sample, os.path.join(HERE, "font.ttf"))

# 2) subset.ttf —— 小固定集合，供 run() 基准使用
bench = ("电子墨水屏阅读器矢量字体任意大小显示嵌入式设备渲染中文需要考虑内存性能平衡"
         "春天来了万物复苏山野间一片新绿他慢慢走过那条熟悉的小路想起许多年前在这里度过的时光"
         "abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789"
         "，。、；：？！“”‘’（）《》—…·")
subset_to(bench, os.path.join(HERE, "subset.ttf"))
print("done.")
