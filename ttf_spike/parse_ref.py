# -*- coding: utf-8 -*-
"""随机访问式 TTF 解析参考实现（与固件将移植的逻辑一致）。
用真正的文件 seek 读取（不整文件载入），逐表解析：表目录→head/maxp/hhea→cmap→loca→hmtx→glyf。
然后用 fontTools 的 glyf 表作为“标准答案”对照，验证我的解析是否正确。
"""
import io, sys
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")
from fontTools.ttLib import TTFont

PATH = r"C:\Windows\Fonts\simhei.ttf"

class R:
    """随机读取器：模拟固件 seek+read。"""
    def __init__(self, path): self.f = open(path, "rb")
    def u8(self, off):
        self.f.seek(off); return self.f.read(1)[0]
    def u16(self, off):
        self.f.seek(off); b = self.f.read(2); return int.from_bytes(b, "big")
    def i16(self, off):
        v = self.u16(off); return v - 0x10000 if v >= 0x8000 else v
    def u32(self, off):
        self.f.seek(off); return int.from_bytes(self.f.read(4), "big")
    def read(self, off, n):
        self.f.seek(off); return self.f.read(n)

r = R(PATH)

# ---- 1. 表目录 ----
num_tables = r.u16(4)
tables = {}
off = 12
for _ in range(num_tables):
    tag = r.read(off, 4).decode("latin1")
    toff = r.u32(off + 8)
    tlen = r.u32(off + 12)
    tables[tag] = (toff, tlen)
    off += 16
print("tables:", sorted(tables.keys()))

head = tables["head"]
units_per_em = r.u16(head[0] + 18)
loc_fmt = r.i16(head[0] + 50)   # 0=short, 1=long
num_glyphs = r.u16(tables["maxp"][0] + 4)
num_h_metrics = r.u16(tables["hhea"][0] + 34)
print(f"units_per_em={units_per_em} num_glyphs={num_glyphs} loc_fmt={'long' if loc_fmt else 'short'} numHMetrics={num_h_metrics}")

loca = tables["loca"]; glyf = tables["glyf"]; hmtx = tables["hmtx"]; cmap = tables["cmap"]

# ---- 2. cmap：找 unicode 子表（优先 format 12） ----
def find_cmap_subtable():
    ver, n = r.u16(cmap[0]), r.u16(cmap[0] + 2)
    best = None
    for i in range(n):
        rec = cmap[0] + 4 + i * 8
        pid, eid, sub_off = r.u16(rec), r.u16(rec + 2), r.u32(rec + 4)
        fmt = r.u16(cmap[0] + sub_off)
        # 偏好 unicode：platform 0 任意，或 platform 3 encoding 1/10
        if pid == 0 or (pid == 3 and eid in (1, 10)):
            score = 2 if fmt == 12 else 1
            if best is None or score > best[0]:
                best = (score, cmap[0] + sub_off, fmt, pid, eid)
    return best
_, cmap_sub, cmap_fmt, _, _ = find_cmap_subtable()
print(f"cmap subtable fmt={cmap_fmt} at {cmap_sub}")

def char_to_gid(ch):
    code = ord(ch)
    if cmap_fmt == 12:
        n_groups = r.u32(cmap_sub + 12)
        base = cmap_sub + 16
        lo, hi = 0, n_groups - 1
        while lo <= hi:
            mid = (lo + hi) // 2
            g = base + mid * 12
            start, end, start_gid = r.u32(g), r.u32(g + 4), r.u32(g + 8)
            if code < start: hi = mid - 1
            elif code > end: lo = mid + 1
            else: return start_gid + (code - start)
    elif cmap_fmt == 4:
        seg_x2 = r.u16(cmap_sub + 6)
        seg = seg_x2 // 2
        end_base = cmap_sub + 14
        pad_base = end_base + seg * 2            # reservedPad
        start_base = pad_base + 2
        delta_base = start_base + seg * 2
        idro_base = delta_base + seg * 2          # idRangeOffset[]
        # 找段：endCode[i] >= code
        for i in range(seg):
            if r.u16(end_base + i * 2) >= code:
                start = r.u16(start_base + i * 2)
                if code < start:
                    return 0
                delta = r.i16(delta_base + i * 2)
                idro = r.u16(idro_base + i * 2)
                if idro == 0:
                    return (code + delta) & 0xFFFF
                gid_addr = idro_base + i * 2 + idro + 2 * (code - start)
                gid = r.u16(gid_addr)
                return (gid + delta) & 0xFFFF if gid else 0
        return 0
    return 0

# ---- 3. loca：gid → glyf 字节范围 ----
def glyf_range(gid):
    if loc_fmt == 1:  # long: u32
        a = r.u32(loca[0] + gid * 4)
        b = r.u32(loca[0] + (gid + 1) * 4)
    else:             # short: u16, 实际偏移×2
        a = r.u16(loca[0] + gid * 2) * 2
        b = r.u16(loca[0] + (gid + 1) * 2) * 2
    return glyf[0] + a, b - a

# ---- 4. hmtx：gid → advanceWidth ----
def advance(gid):
    if gid < num_h_metrics:
        return r.u16(hmtx[0] + gid * 4)
    return r.u16(hmtx[0] + (num_h_metrics - 1) * 4)

# ---- 5. glyf simple 解析 → (坐标, endPts, flags) ----
def parse_simple(gid):
    goff, glen = glyf_range(gid)
    if glen == 0:
        return None  # 空字形
    p = goff
    n_contours = r.i16(p); p += 2
    if n_contours < 0:
        return ("composite", None)  # 复合字形，先跳过
    # xMin,yMin,xMax,yMax 跳过
    p += 8
    end_pts = [r.u16(p + 2 * i) for i in range(n_contours)]; p += 2 * n_contours
    n_pts = end_pts[-1] + 1 if end_pts else 0
    instr_len = r.u16(p); p += 2 + instr_len
    # flags（带 repeat）
    flags = []
    while len(flags) < n_pts:
        f = r.u8(p); p += 1
        flags.append(f)
        if f & 0x08:
            rep = r.u8(p); p += 1
            flags += [f] * rep
    flags = flags[:n_pts]
    # x
    xs = []; x = 0
    for f in flags:
        if f & 0x02:            # X_SHORT
            dx = r.u8(p); p += 1
            x += dx if (f & 0x10) else -dx
        else:
            if not (f & 0x10):  # 不是 X_SAME → 读 delta
                x += r.i16(p); p += 2
        xs.append(x)
    # y
    ys = []; y = 0
    for f in flags:
        if f & 0x04:            # Y_SHORT
            dy = r.u8(p); p += 1
            y += dy if (f & 0x20) else -dy
        else:
            if not (f & 0x20):
                y += r.i16(p); p += 2
        ys.append(y)
    coords = list(zip(xs, ys))
    on_curve = [bool(fl & 0x01) for fl in flags]
    return (coords, end_pts, on_curve)

# ---- 对照 fontTools ----
ft = TTFont(PATH)
ft_cmap = ft.getBestCmap()
ft_glyf = ft["glyf"]

tests = "电子墨水屏矢量字体"
allok = True
for ch in tests:
    gname = ft_cmap.get(ord(ch))
    gid_mine = char_to_gid(ch)
    mine = parse_simple(gid_mine)
    if mine is None or mine[0] == "composite":
        print(f"{ch}: gid={gid_mine} 空/复合，跳过"); continue
    coords_m, end_m, on_m = mine
    # fontTools 对照
    g = ft_glyf[gname]
    coords_ft = [(int(x), int(y)) for x, y in g.coordinates]
    end_ft = list(g.endPtsOfContours)
    on_ft = [bool(f) for f in g.flags]
    ok = (coords_m == coords_ft) and (end_m == end_ft) and (on_m == on_ft)
    allok &= ok
    print(f"{ch}: gid={gid_mine} pts={len(coords_m)} "
          f"adv_mine={advance(gid_mine)} adv_ft={ft['hmtx'][gname][0]} "
          f"{'OK' if ok else 'MISMATCH'}")
    if not ok:
        print(f"   mine end={end_m[:5]} ft={end_ft[:5]}")
        print(f"   mine coords[:3]={coords_m[:3]} ft[:3]={coords_ft[:3]}")
print("\n全部匹配：" + ("是 ✓ 解析逻辑正确，可移植到 Rust" if allok else "否 ✗"))
