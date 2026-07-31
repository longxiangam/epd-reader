# -*- coding: utf-8 -*-
import time; print("START", flush=True)
"""端到端验证：随机访问解析 → 轮廓 → 展平二次曲线 → 奇偶扫描线填充 → 位图。
对照 PIL(freetype) 渲染同一字，比较二值掩码重合度（IoU）。
全部用“固件将要移植的同一算法”，验证通过即可移植 Rust。
"""
import io, sys
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")
from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFont

PATH = r"C:\Windows\Fonts\simhei.ttf"
PX = 32  # 测试字号

# ---- 随机读取器 ----
class R:
    def __init__(self, p): self.f = open(p, "rb")
    def u8(self, o): self.f.seek(o); return self.f.read(1)[0]
    def u16(self, o): self.f.seek(o); return int.from_bytes(self.f.read(2), "big")
    def i16(self, o): v = self.u16(o); return v - 0x10000 if v >= 0x8000 else v
    def u32(self, o): self.f.seek(o); return int.from_bytes(self.f.read(4), "big")
    def read(self, o, n): self.f.seek(o); return self.f.read(n)
r = R(PATH)
num_tables = r.u16(4); tables = {}; off = 12
for _ in range(num_tables):
    tag = r.read(off, 4).decode("latin1"); tables[tag] = (r.u32(off+8), r.u32(off+12)); off += 16
head = tables["head"]; UP = r.u16(head[0]+18); LOC_FMT = r.i16(head[0]+50)
NG = r.u16(tables["maxp"][0]+4); NHM = r.u16(tables["hhea"][0]+34)
loca, glyf, hmtx, cmap = tables["loca"], tables["glyf"], tables["hmtx"], tables["cmap"]
# cmap 子表
ver, nrec = r.u16(cmap[0]), r.u16(cmap[0]+2); best = None
for i in range(nrec):
    rec = cmap[0]+4+i*8; pid, eid, so = r.u16(rec), r.u16(rec+2), r.u32(rec+4)
    fmt = r.u16(cmap[0]+so)
    if pid == 0 or (pid == 3 and eid in (1,10)):
        s = 2 if fmt == 12 else 1
        if best is None or s > best[0]: best = (s, cmap[0]+so, fmt)
_, CSUB, CFMT = best

def c2g(ch):
    c = ord(ch)
    if CFMT == 12:
        ng = r.u32(CSUB+12); base = CSUB+16; lo, hi = 0, ng-1
        while lo <= hi:
            m = (lo+hi)//2; g = base+m*12; st, en, sg = r.u32(g), r.u32(g+4), r.u32(g+8)
            if c < st: hi = m-1
            elif c > en: lo = m+1
            else: return sg + (c - st)
    else:
        sx2 = r.u16(CSUB+6); seg = sx2//2
        eb = CSUB+14; sb = eb+seg*2+2; db = sb+seg*2; rb = db+seg*2
        for i in range(seg):
            if r.u16(eb+i*2) >= c:
                if r.u16(sb+i*2) > c: return 0
                d = r.i16(db+i*2); ro = r.u16(rb+i*2)
                if ro == 0: return (c+d) & 0xFFFF
                gid = r.u16(rb+i*2+ro+2*(c-r.u16(sb+i*2)))
                return (gid+d) & 0xFFFF if gid else 0
    return 0

def grange(gid):
    if LOC_FMT == 1: a = r.u32(loca[0]+gid*4); b = r.u32(loca[0]+(gid+1)*4)
    else: a = r.u16(loca[0]+gid*2)*2; b = r.u16(loca[0]+(gid+1)*2)*2
    return glyf[0]+a, b-a

def parse(gid):
    go, gl = grange(gid)
    if gl == 0: return None
    p = go; nc = r.i16(p); p += 2
    if nc < 0: return None
    x0, y0, x1, y1 = r.i16(p), r.i16(p+2), r.i16(p+4), r.i16(p+6); p += 8
    ep = [r.u16(p+2*i) for i in range(nc)]; p += 2*nc
    npt = ep[-1]+1 if ep else 0
    il = r.u16(p); p += 2+il
    fl = []
    while len(fl) < npt:
        f = r.u8(p); p += 1; fl.append(f)
        if f & 8: fl += [f]*r.u8(p); p += 1
    fl = fl[:npt]
    xs = []; x = 0
    for f in fl:
        if f & 2: dx = r.u8(p); p += 1; x += dx if (f & 0x10) else -dx
        elif not (f & 0x10): x += r.i16(p); p += 2
        xs.append(x)
    ys = []; y = 0
    for f in fl:
        if f & 4: dy = r.u8(p); p += 1; y += dy if (f & 0x20) else -dy
        elif not (f & 0x20): y += r.i16(p); p += 2
        ys.append(y)
    return (list(zip(xs, ys)), ep, [bool(f & 1) for f in fl], (x0,y0,x1,y1))

def contour_segments(pts, on):
    """TT 轮廓 → 线段列表（二次曲线展平）。旋转数组使起点为 on-curve，按计数遍历 n 点。"""
    n = len(pts)
    if n == 0: return []
    segs = []
    start = 0
    while start < n and not on[start]: start += 1
    if start == n:
        # 全 off：合成起点，相邻中点为隐式 on
        vcur = ((pts[-1][0]+pts[0][0])/2, (pts[-1][1]+pts[0][1])/2)
        first = vcur
        for k in range(n):
            ctrl = pts[k]; nxt = pts[(k+1)%n]
            end = ((ctrl[0]+nxt[0])/2, (ctrl[1]+nxt[1])/2)
            _quad(segs, vcur, ctrl, end); vcur = end
        return segs
    # 旋转使 P[0] 为 on-curve 起点
    P = pts[start:] + pts[:start]
    F = on[start:] + on[:start]
    vcur = P[0]
    k = 1
    while k < n:
        if F[k]:
            segs.append((vcur[0], vcur[1], P[k][0], P[k][1])); vcur = P[k]; k += 1
        else:
            ctrl = P[k]
            if k + 1 < n and F[k+1]:
                end = P[k+1]; _quad(segs, vcur, ctrl, end); vcur = end; k += 2
            else:
                nxt = P[k+1] if k+1 < n else P[0]
                end = ((ctrl[0]+nxt[0])/2, (ctrl[1]+nxt[1])/2)
                _quad(segs, vcur, ctrl, end); vcur = end; k += 1
    # 闭合回起点
    if (vcur[0], vcur[1]) != (P[0][0], P[0][1]):
        segs.append((vcur[0], vcur[1], P[0][0], P[0][1]))
    return segs

def _quad(out, p0, p1, p2, N=6):
    """二次贝塞尔 p0->p1->p2 展平成 N 段线段（像素坐标）。"""
    px, py = p0
    for k in range(1, N+1):
        t = k/N; mt = 1-t
        x = mt*mt*p0[0] + 2*mt*t*p1[0] + t*t*p2[0]
        y = mt*mt*p0[1] + 2*mt*t*p1[1] + t*t*p2[1]
        out.append((px, py, x, y)); px, py = x, y

def render(gid, px):
    g = parse(gid)
    if g is None: return None
    pts, ep, on, (x0,y0,x1,y1) = g
    sc = px / UP
    # 像素 bbox
    w = max(1, int(round((x1-x0)*sc))+1); h = max(1, int(round((y1-y0)*sc))+1)
    def tx(X): return (X - x0) * sc
    def ty(Y): return (y1 - Y) * sc   # y 翻转
    # 收集所有线段（像素坐标）
    segs = []
    si = 0
    for ci in range(len(ep)):
        ei = ep[ci]
        cpts = [(tx(pts[k][0]), ty(pts[k][1])) for k in range(si, ei+1)]
        con = [on[k] for k in range(si, ei+1)]
        segs.extend(contour_segments(cpts, con))
        si = ei+1
    # 奇偶扫描线填充
    img = Image.new("1", (w, h), 0); pxl = img.load()
    for y in range(h):
        yc = y + 0.5
        xs = []
        for (ax, ay, bx, by) in segs:
            if (ay <= yc <= by) or (by <= yc <= ax and False):
                pass
            if (ay <= yc < by) or (by <= yc < ay):
                t = (yc - ay) / (by - ay) if by != ay else 0
                xs.append(ax + t * (bx - ax))
        xs.sort()
        for k in range(0, len(xs)-1, 2):
            xstart = int(max(0, xs[k])); xend = int(min(w-1, xs[k+1]))
            for xx in range(xstart, xend+1): pxl[xx, y] = 1
    return img

# ---- 对照 PIL（先单字计时定位） ----
import time
pil_font = ImageFont.truetype(PATH, PX)
total_iou = 0; cnt = 0
for ch in "电子墨水矢量字体":
    t = time.time(); gid = c2g(ch); t_c2g = time.time()-t
    t = time.time(); mine = render(gid, PX); t_render = time.time()-t
    print(f"{ch}: gid={gid} c2g={t_c2g:.3f}s render={t_render:.3f}s", flush=True)
    if mine is None:
        print(f"{ch}: 空字形"); continue
    # PIL 渲染同字
    pil_img = Image.new("1", (mine.width+4, mine.height+4), 0)
    ImageDraw.Draw(pil_img).text((2, 2), ch, font=pil_font, fill=1)
    pil_crop = pil_img.crop((0,0,mine.width, mine.height))
    # IoU
    a = mine.load(); b = pil_crop.load()
    inter = uni = 0
    for yy in range(mine.height):
        for xx in range(mine.width):
            av = a[xx,yy]; bv = b[xx,yy] if xx < pil_crop.width and yy < pil_crop.height else 0
            if av and bv: inter += 1
            if av or bv: uni += 1
    iou = inter/uni if uni else 0
    total_iou += iou; cnt += 1
    print(f"{ch}: gid={gid} mine={mine.width}x{mine.height} IoU={iou:.2f}")
print(f"\n平均 IoU = {total_iou/cnt:.2f}  (>=0.85 视为渲染正确)")
mine.save("ttf_spike/_mine.png")
pil_crop.save("ttf_spike/_pil.png")
print("样图：ttf_spike/_mine.png (我的) vs _pil.png (PIL/freetype)")

# ---- ASCII 预览 ----
print("\n=== ASCII 预览（█=填充）===")
for ch in "电子字":
    img = render(c2g(ch), 32)
    if img is None: print(ch,"空"); continue
    px = img.load()
    print(f"--- {ch} {img.width}x{img.height} ---")
    for yy in range(img.height):
        print("".join("█" if px[xx,yy] else "·" for xx in range(img.width)))
