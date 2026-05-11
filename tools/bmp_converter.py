"""
EPD Reader 睡眠图片转换工具
将普通图片转换为 300x400 1-bit BMP，用于电子墨水屏睡眠界面。
依赖: pip install Pillow
"""

import tkinter as tk
from tkinter import filedialog, messagebox
from PIL import Image, ImageTk


class BmpConverterApp:
    TARGET_W = 300
    TARGET_H = 400

    def __init__(self, root: tk.Tk):
        self.root = root
        self.root.title("EPD 睡眠图片转换工具")
        self.root.resizable(False, False)
        self.source_image: Image.Image | None = None
        self.result_image: Image.Image | None = None
        self.source_photo: ImageTk.PhotoImage | None = None
        self.result_photo: ImageTk.PhotoImage | None = None
        self._build_ui()

    def _build_ui(self):
        # --- top bar ---
        top = tk.Frame(self.root)
        top.pack(fill="x", padx=8, pady=4)

        tk.Button(top, text="选择图片", command=self._on_open).pack(side="left")
        tk.Button(top, text="保存 BMP", command=self._on_save).pack(side="left", padx=(8, 0))

        # --- dither option ---
        opt_frame = tk.LabelFrame(self.root, text="二值化方式")
        opt_frame.pack(fill="x", padx=8, pady=4)

        self.dither_var = tk.StringVar(value="floyd")
        for text, val in [("Floyd-Steinberg 抖动（推荐）", "floyd"),
                          ("简单阈值", "threshold")]:
            tk.Radiobutton(opt_frame, text=text, variable=self.dither_var,
                           value=val, command=self._refresh_preview).pack(anchor="w")

        # --- threshold slider ---
        self.threshold_frame = tk.Frame(opt_frame)
        self.threshold_frame.pack(anchor="w", padx=20)
        tk.Label(self.threshold_frame, text="阈值:").pack(side="left")
        self.threshold_var = tk.IntVar(value=128)
        self.threshold_scale = tk.Scale(self.threshold_frame, from_=0, to=255,
                                        orient="horizontal", variable=self.threshold_var,
                                        command=lambda _: self._refresh_preview())
        self.threshold_scale.pack(side="left")

        # --- invert option ---
        self.invert_var = tk.BooleanVar(value=False)
        tk.Checkbutton(opt_frame, text="反色（黑白翻转）", variable=self.invert_var,
                       command=self._refresh_preview).pack(anchor="w", padx=20)

        # --- preview ---
        preview_frame = tk.Frame(self.root)
        preview_frame.pack(fill="both", expand=True, padx=8, pady=4)

        # source
        src_col = tk.LabelFrame(preview_frame, text="原图")
        src_col.pack(side="left", expand=True, fill="both", padx=(0, 4))
        self.src_label = tk.Label(src_col, text="未选择图片", width=36, height=20,
                                  bg="#f0f0f0")
        self.src_label.pack(padx=4, pady=4)

        # result
        res_col = tk.LabelFrame(preview_frame, text="预览 (300x400 1-bit)")
        res_col.pack(side="left", expand=True, fill="both", padx=(4, 0))
        self.res_label = tk.Label(res_col, text="等待转换", width=36, height=20,
                                  bg="#f0f0f0")
        self.res_label.pack(padx=4, pady=4)

        # --- status ---
        self.status_var = tk.StringVar(value="请选择一张图片")
        tk.Label(self.root, textvariable=self.status_var, anchor="w",
                 relief="sunken").pack(fill="x", padx=8, pady=(0, 8))

    # ---- actions ----

    def _on_open(self):
        path = filedialog.askopenfilename(
            title="选择图片",
            filetypes=[("图片", "*.png *.jpg *.jpeg *.bmp *.gif *.webp"), ("所有文件", "*.*")]
        )
        if not path:
            return
        try:
            self.source_image = Image.open(path).convert("RGB")
        except Exception as e:
            messagebox.showerror("错误", f"无法打开图片:\n{e}")
            return
        self.status_var.set(f"已加载: {path}  ({self.source_image.size[0]}x{self.source_image.size[1]})")
        self._show_source()
        self._refresh_preview()

    def _on_save(self):
        if self.result_image is None:
            messagebox.showwarning("提示", "请先选择图片")
            return
        path = filedialog.asksaveasfilename(
            title="保存 BMP",
            defaultextension=".bmp",
            initialfile="sleep.bmp",
            filetypes=[("BMP 图片", "*.bmp")]
        )
        if not path:
            return
        try:
            self.result_image.save(path, format="BMP")
            self.status_var.set(f"已保存: {path}")
        except Exception as e:
            messagebox.showerror("错误", f"保存失败:\n{e}")

    # ---- preview helpers ----

    def _preview_size(self, img: Image.Image, max_w=220, max_h=300):
        w, h = img.size
        scale = min(max_w / w, max_h / h, 1.0)
        return int(w * scale), int(h * scale)

    def _show_source(self):
        if self.source_image is None:
            return
        display = self.source_image.copy()
        pw, ph = self._preview_size(display)
        display = display.resize((pw, ph), Image.LANCZOS)
        self.source_photo = ImageTk.PhotoImage(display)
        self.src_label.configure(image=self.source_photo, text="", width=pw, height=ph)

    def _refresh_preview(self):
        if self.source_image is None:
            return
        self.result_image = self._convert(self.source_image)
        # scale up 1-bit for preview so it's visible
        display = self.result_image.convert("RGB").resize(
            (self.TARGET_W, self.TARGET_H), Image.NEAREST
        )
        pw, ph = self._preview_size(display)
        display = display.resize((pw, ph), Image.NEAREST)
        self.result_photo = ImageTk.PhotoImage(display)
        self.res_label.configure(image=self.result_photo, text="", width=pw, height=ph)

    def _convert(self, img: Image.Image) -> Image.Image:
        resized = img.resize((self.TARGET_W, self.TARGET_H), Image.LANCZOS)
        gray = resized.convert("L")

        method = self.dither_var.get()
        if method == "floyd":
            result = gray.convert("1")
        else:
            threshold = self.threshold_var.get()
            result = gray.point(lambda p: 255 if p > threshold else 0, "1")

        if self.invert_var.get():
            result = Image.eval(result, lambda p: 255 - p)

        return result


def main():
    root = tk.Tk()
    BmpConverterApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()
