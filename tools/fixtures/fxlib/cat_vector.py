"""Categories: vector-heavy, cad, transparency."""

from __future__ import annotations

import math
import struct
from pathlib import Path

from .common import Ctx, Job, rng
from .rawpdf import Content, RawPdf, Ref, add_page, num, pdf_lit, simple_document, std_font
from .rl import A4, rl_canvas

A0_LAND = (3370.3937, 2383.937)
A1_LAND = (2383.937, 1683.7795)
A3_PORT = (841.8898, 1190.5512)


def jet(t: float) -> tuple[float, float, float]:
    t = min(1.0, max(0.0, t))
    return (min(1.0, max(0.0, 1.5 - abs(4 * t - 3))), min(1.0, max(0.0, 1.5 - abs(4 * t - 2))),
            min(1.0, max(0.0, 1.5 - abs(4 * t - 1))))


def f2(x: float) -> str:
    return f"{x:.2f}"


# --------------------------------------------------------------------------
# vector-heavy
# --------------------------------------------------------------------------
def vec_bezier(out: Path, ctx: Ctx, pages: int = 4, per_page: int = 10000) -> dict:
    r = rng("vec-bezier")
    W, H = A4
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica-Bold"))
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        for p in range(pages):
            lines = ["1 J 1 j"]
            for _ in range(per_page):
                cr, cg, cb = jet(r.random())
                x0, y0 = r.uniform(0, W), r.uniform(0, H)
                pts = [(x0 + r.uniform(-120, 120), y0 + r.uniform(-120, 120)) for _ in range(3)]
                lines.append(f"{cr:.3f} {cg:.3f} {cb:.3f} RG {r.uniform(0.1, 2.5):.2f} w "
                             f"{f2(x0)} {f2(y0)} m " + " ".join(f"{f2(x)} {f2(y)}" for x, y in pts)
                             + " c S")
            lines.append(f"BT /F1 18 Tf 0 g 30 30 Td (Bezier stress page {p + 1}: "
                         f"{per_page} stroked cubic curves) Tj ET")
            add_page(pdf, pages_ref, kids, "\n".join(lines).encode("ascii"),
                     {"Font": {"F1": font}})
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: bezier curves",
                        id_seed="vec-bezier")
    return {"pages": pages}


def vec_fills_gradients(out: Path, ctx: Ctx) -> dict:
    """reportlab: nonzero/even-odd polygon fills, gradients in clips, dashes, joins."""
    from reportlab.lib.colors import Color
    from reportlab.pdfgen.canvas import FILL_EVEN_ODD, FILL_NON_ZERO

    r = rng("vec-fills")
    c = rl_canvas(out, "FastPDF fixture: fills, gradients, strokes")
    W, H = A4
    # Page 1: 2000 random star polygons, alternating fill rules
    for i in range(2000):
        cx, cy = r.uniform(0, W), r.uniform(0, H)
        n = r.choice((5, 7, 9))
        rad = r.uniform(6, 40)
        p = c.beginPath()
        step = 2 if n != 9 else 4
        for k in range(n + 1):
            a = 2 * math.pi * ((k * step) % n) / n
            x, y = cx + rad * math.cos(a), cy + rad * math.sin(a)
            (p.moveTo if k == 0 else p.lineTo)(x, y)
        p.close()
        c.setFillColorRGB(*jet(r.random()))
        c.setStrokeColorRGB(0, 0, 0)
        c.setLineWidth(0.3)
        c.drawPath(p, stroke=1, fill=1, fillMode=FILL_EVEN_ODD if i % 2 else FILL_NON_ZERO)
    c.setFillColorRGB(0, 0, 0)
    c.setFont("Helvetica-Bold", 14)
    c.drawString(30, 20, "2000 self-intersecting polygons, nonzero / even-odd fill rules")
    c.showPage()
    # Page 2: linear and radial gradients inside clipping paths
    for i in range(12):
        col, row = i % 3, i // 3
        x, y = 40 + col * 180, H - 220 - row * 190
        c.saveState()
        p = c.beginPath()
        if i % 3 == 0:
            p.circle(x + 80, y + 80, 78)
        elif i % 3 == 1:
            p.roundRect(x, y, 160, 160, 24)
        else:
            p.moveTo(x, y)
            p.curveTo(x + 40, y + 200, x + 120, y - 40, x + 160, y + 160)
            p.lineTo(x + 160, y)
            p.close()
        c.clipPath(p, stroke=0, fill=0)
        cols = [Color(*jet(r.random())) for _ in range(2 + i % 3)]
        if i % 2:
            c.radialGradient(x + 80, y + 80, 90, cols, extend=False)
        else:
            c.linearGradient(x, y, x + 160, y + 160, cols, extend=True)
        c.restoreState()
    c.setFont("Helvetica-Bold", 14)
    c.drawString(30, 20, "Axial / radial shadings (sh) clipped to curves")
    c.showPage()
    # Page 3: stroke styles
    y = H - 60
    for cap in range(3):
        for join in range(3):
            c.setLineCap(cap)
            c.setLineJoin(join)
            c.setLineWidth(9)
            c.setStrokeColorRGB(0.2, 0.3, 0.7)
            p = c.beginPath()
            p.moveTo(60, y)
            p.lineTo(160, y - 40)
            p.lineTo(260, y)
            p.lineTo(300, y - 30)
            c.drawPath(p, stroke=1, fill=0)
            c.setFont("Helvetica", 9)
            c.drawString(320, y - 20, f"cap={cap} join={join}")
            y -= 60
    c.setLineCap(0)
    for k, dash in enumerate(([], [6, 3], [1, 2], [12, 4, 2, 4], [0.5, 6])):
        c.setDash(dash, k)
        c.setLineWidth(1.5)
        c.line(60, y, 520, y)
        y -= 18
    c.setDash([])
    for k in range(10):
        c.setMiterLimit(1 + k)
        c.setLineJoin(0)
        c.setLineWidth(5)
        p = c.beginPath()
        x0 = 60 + k * 48
        p.moveTo(x0, 60)
        p.lineTo(x0 + 20, 60 + 30 + k * 4)
        p.lineTo(x0 + 40, 60)
        c.drawPath(p, stroke=1, fill=0)
    c.showPage()
    c.save()
    return {"pages": 3}


def _mesh_bytes(vals: list[tuple[int, ...]], fmt: str) -> bytes:
    return b"".join(struct.pack(fmt, *v) for v in vals)


def vec_shadings_patterns(out: Path, ctx: Ctx) -> dict:
    """All seven shading types plus tiling / shading patterns."""
    W, H = A4
    tw, th = 240.0, 140.0
    q16 = 65535

    def q(v: float, lo: float, hi: float) -> int:
        return max(0, min(q16, round((v - lo) / (hi - lo) * q16)))

    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica-Bold"))
        rgb = "DeviceRGB"
        sh: dict[str, Ref] = {}
        f4 = pdf.stream(b"{ 2 copy mul 3 1 roll }", {"FunctionType": 4, "Domain": [0, 1, 0, 1],
                                                      "Range": [0, 1, 0, 1, 0, 1]})
        sh["S1"] = pdf.obj({"ShadingType": 1, "ColorSpace": rgb, "Domain": [0, 1, 0, 1],
                            "Matrix": [tw, 0, 0, th, 0, 0], "Function": f4})
        stitch = {"FunctionType": 3, "Domain": [0, 1], "Bounds": [0.33, 0.66],
                  "Encode": [0, 1, 0, 1, 0, 1],
                  "Functions": [{"FunctionType": 2, "Domain": [0, 1], "C0": a, "C1": b, "N": 1}
                                for a, b in (([1, 0, 0], [1, 1, 0]), ([1, 1, 0], [0, 1, 0]),
                                             ([0, 1, 0], [0, 0, 1]))]}
        sh["S2"] = pdf.obj({"ShadingType": 2, "ColorSpace": rgb, "Coords": [0, 0, tw, th],
                            "Function": stitch, "Extend": [True, True]})
        sh["S3"] = pdf.obj({"ShadingType": 3, "ColorSpace": rgb,
                            "Coords": [tw * 0.35, th * 0.6, 5, tw * 0.5, th * 0.5, 110],
                            "Function": {"FunctionType": 2, "Domain": [0, 1], "C0": [1, 1, 1],
                                         "C1": [0.1, 0.2, 0.6], "N": 1.5},
                            "Extend": [True, True]})
        dec = [0, tw, 0, th, 0, 1, 0, 1, 0, 1]
        # type 4: free-form triangle mesh (each triangle restarts with flag 0)
        r = rng("vec-mesh")
        verts = []
        for _ in range(60):
            cx, cy = r.uniform(0, tw), r.uniform(0, th)
            for _ in range(3):
                verts.append((0, q(min(tw, max(0, cx + r.uniform(-40, 40))), 0, tw),
                              q(min(th, max(0, cy + r.uniform(-40, 40))), 0, th),
                              r.randrange(256), r.randrange(256), r.randrange(256)))
        sh["S4"] = pdf.stream(_mesh_bytes(verts, ">BHHBBB"),
                              {"ShadingType": 4, "ColorSpace": rgb, "BitsPerCoordinate": 16,
                               "BitsPerComponent": 8, "BitsPerFlag": 8, "Decode": dec})
        # type 5: lattice-form mesh
        rows, per_row = 12, 16
        lat = []
        for j in range(rows):
            for i in range(per_row):
                x = tw * i / (per_row - 1)
                y = th * j / (rows - 1) + 6 * math.sin(i * 0.8)
                cr, cg, cb = jet((i + j) / (rows + per_row - 2))
                lat.append((q(x, 0, tw), q(min(th, max(0, y)), 0, th),
                            round(cr * 255), round(cg * 255), round(cb * 255)))
        sh["S5"] = pdf.stream(_mesh_bytes(lat, ">HHBBB"),
                              {"ShadingType": 5, "ColorSpace": rgb, "BitsPerCoordinate": 16,
                               "BitsPerComponent": 8, "VerticesPerRow": per_row, "Decode": dec})

        def patch_points(tensor: bool) -> list[tuple[float, float]]:
            def p(i: int, j: int) -> tuple[float, float]:
                x = tw * (0.05 + 0.9 * i / 3) + (12 if (i + j) % 2 else -12) * (0 < j < 3)
                y = th * (0.05 + 0.9 * j / 3) + (10 if i % 2 else -10) * (0 < i < 3)
                return x, y
            order = [(0, 0), (0, 1), (0, 2), (0, 3), (1, 3), (2, 3), (3, 3), (3, 2), (3, 1),
                     (3, 0), (2, 0), (1, 0)]
            if tensor:
                order += [(1, 1), (1, 2), (2, 2), (2, 1)]
            return [p(i, j) for i, j in order]

        colors = [(255, 0, 0), (0, 200, 0), (0, 0, 255), (255, 220, 0)]
        for name, stype, tensor in (("S6", 6, False), ("S7", 7, True)):
            pts = patch_points(tensor)
            data = bytes([0]) + b"".join(struct.pack(">HH", q(x, 0, tw), q(y, 0, th))
                                         for x, y in pts) + bytes(c for col in colors for c in col)
            sh[name] = pdf.stream(data, {"ShadingType": stype, "ColorSpace": rgb,
                                         "BitsPerCoordinate": 16, "BitsPerComponent": 8,
                                         "BitsPerFlag": 8, "Decode": dec})
        # patterns
        tile = pdf.stream(b"0.95 0.6 0.1 rg 2 2 8 8 re f 0.1 0.4 0.8 RG 1 w 0 0 m 20 20 l S",
                          {"Type": "Pattern", "PatternType": 1, "PaintType": 1, "TilingType": 1,
                           "BBox": [0, 0, 20, 20], "XStep": 20, "YStep": 20, "Resources": {}})
        unc = pdf.stream(b"5 5 m 15 5 l 10 15 l h f",
                         {"Type": "Pattern", "PatternType": 1, "PaintType": 2, "TilingType": 2,
                          "BBox": [0, 0, 20, 20], "XStep": 20, "YStep": 20, "Resources": {}})
        shp = pdf.obj({"Type": "Pattern", "PatternType": 2, "Shading": sh["S2"],
                       "Matrix": [2, 0, 0, 0.6, 40, 80]})
        res = {"Font": {"F1": font}, "Shading": sh,
               "Pattern": {"P1": tile, "P2": unc, "P3": shp},
               "ColorSpace": {"CSp": ["Pattern", "DeviceRGB"]}}
        cs = Content()
        labels = {"S1": "Type 1 function-based (Type 4 PostScript fn)",
                  "S2": "Type 2 axial (Type 3 stitching fn)", "S3": "Type 3 radial",
                  "S4": "Type 4 free-form triangle mesh", "S5": "Type 5 lattice mesh",
                  "S6": "Type 6 Coons patch", "S7": "Type 7 tensor-product patch"}
        for k, (name, label) in enumerate(labels.items()):
            col, row = k % 2, k // 2
            x, y = 40 + col * (tw + 30), H - 200 - row * (th + 40)
            cs(f"q 1 0 0 1 {num(x)} {num(y)} cm 0 0 {num(tw)} {num(th)} re W n /{name} sh Q")
            cs(f"0 G 0.5 w {num(x)} {num(y)} {num(tw)} {num(th)} re S")
            cs(f"BT /F1 9 Tf 0 g {num(x)} {num(y - 11)} Td {pdf_lit(label.encode())} Tj ET")
        x, y = 40 + tw + 30, H - 200 - 3 * (th + 40)
        cs(f"/Pattern cs /P1 scn {num(x)} {num(y)} {num(tw / 2 - 5)} {num(th)} re f")
        cs(f"/CSp cs 0.2 0.5 0.9 /P2 scn {num(x + tw / 2 + 5)} {num(y)} {num(tw / 2 - 5)} {num(th)} re f")
        cs(f"BT /F1 9 Tf 0 g {num(x)} {num(y - 11)} Td (Tiling patterns: coloured / uncoloured) Tj ET")
        cs("BT /Pattern cs /P3 scn /F1 40 Tf 40 30 Td (SHADING PATTERN TEXT) Tj ET")
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, cs.data(), res)
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: shading types 1-7, patterns",
                        id_seed="vec-shadings")
    return {"pages": 1}


def vec_polyline_map(out: Path, ctx: Ctx, lines_n: int = 300, pts_n: int = 1000) -> dict:
    r = rng("vec-map")
    W, H = A3_PORT
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica"))
        cs = Content()
        cs("0.85 G 0.3 w [4 2] 0 d")
        for x in range(0, int(W), 40):
            cs(f"{x} 0 m {x} {num(H)} l S")
        for y in range(0, int(H), 40):
            cs(f"0 {y} m {num(W)} {y} l S")
        cs("[] 0 d 1 j 1 J")
        for i in range(lines_n):
            base = 20 + (H - 40) * i / lines_n
            amp = r.uniform(4, 30)
            ph = r.uniform(0, 6.28)
            fr = r.uniform(0.004, 0.02)
            cr, cg, cb = jet(i / lines_n)
            pts = []
            y = base
            for k in range(pts_n):
                x = W * k / (pts_n - 1)
                y = base + amp * math.sin(fr * x + ph) + r.uniform(-1.2, 1.2)
                pts.append(f"{f2(x)} {f2(y)} l")
            pts[0] = pts[0][:-1] + "m"
            cs(f"{cr:.3f} {cg:.3f} {cb:.3f} RG {0.2 + (i % 5) * 0.15:.2f} w " + " ".join(pts) + " S")
            if i % 10 == 0:
                cs(f"BT /F1 5 Tf 0 g {f2(W - 60)} {f2(base + 2)} Td (contour {i}) Tj ET")
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, cs.data(), {"Font": {"F1": font}}, [0, 0, W, H])
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: dense polyline map",
                        id_seed="vec-map")
    return {"pages": 1, "segments": lines_n * (pts_n - 1)}


# --------------------------------------------------------------------------
# cad
# --------------------------------------------------------------------------
def _title_block(cs: Content, W: float, title: str, dwg: str, scale: str) -> None:
    cs("0 G 2 w 20 20 m %s 20 l S" % f2(W - 20))
    x0 = W - 620
    cs(f"0 G 1 w {f2(x0)} 20 600 140 re S {f2(x0)} 90 m {f2(W - 20)} 90 l S "
       f"{f2(x0 + 400)} 20 m {f2(x0 + 400)} 160 l S")
    cs(f"BT /F2 22 Tf {f2(x0 + 12)} 120 Td {pdf_lit(title.encode())} Tj ET")
    cs(f"BT /F1 10 Tf {f2(x0 + 12)} 60 Td (FICTITIOUS PROJECT - SYNTHETIC CAD FIXTURE) Tj ET")
    cs(f"BT /F1 10 Tf {f2(x0 + 12)} 40 Td (Generated by tools/fixtures/generate.py) Tj ET")
    cs(f"BT /F2 14 Tf {f2(x0 + 412)} 120 Td {pdf_lit(('DWG ' + dwg).encode())} Tj ET")
    cs(f"BT /F1 12 Tf {f2(x0 + 412)} 60 Td {pdf_lit(('SCALE ' + scale).encode())} Tj ET")


def cad_floorplan(out: Path, ctx: Ctx) -> dict:
    """A0 floor plan: walls, hatching, furniture, dimensions, grid; OCG layers."""
    r = rng("cad-floorplan")
    W, H = A0_LAND
    seg = 0
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        f1 = pdf.obj(std_font("Helvetica"))
        f2_ = pdf.obj(std_font("Helvetica-Bold"))
        layer_names = ["GRID", "WALLS", "HATCH", "FURNITURE", "DIMENSIONS", "TEXT"]
        ocgs = {n: pdf.obj({"Type": "OCG", "Name": n.encode()}) for n in layer_names}
        cs = Content()
        cs(f"0 G 3 w 20 20 {f2(W - 40)} {f2(H - 40)} re S")
        _title_block(cs, W, "FLOOR PLAN - LEVEL 1", "FX-A0-0001", "1:100")
        ox, oy = 120.0, 220.0
        cols, rows = 12, 8
        rw, rh = (W - 2 * ox) / cols, (H - oy - 120) / rows
        # GRID layer
        cs("/OC /L0 BDC 0.75 G 0.15 w [8 4 2 4] 0 d")
        for i in range(cols + 1):
            x = ox + i * rw
            cs(f"{f2(x)} {f2(oy - 60)} m {f2(x)} {f2(H - 80)} l S")
            cs(f"BT /F2 14 Tf {f2(x - 5)} {f2(oy - 85)} Td ({chr(65 + i)}) Tj ET")
            seg += 1
        for j in range(rows + 1):
            y = oy + j * rh
            cs(f"{f2(ox - 60)} {f2(y)} m {f2(W - 80)} {f2(y)} l S")
            cs(f"BT /F2 14 Tf {f2(ox - 95)} {f2(y - 5)} Td ({j + 1}) Tj ET")
            seg += 1
        cs("[] 0 d EMC")
        # WALLS layer: double lines with door gaps and door swing arcs
        cs("/OC /L1 BDC 0 G 0.5 w")
        t = 6.0
        k = 0.5523
        for i in range(cols):
            for j in range(rows):
                x, y = ox + i * rw, oy + j * rh
                door = r.uniform(0.25, 0.6) * rw
                for off in (0.0, t):
                    cs(f"{f2(x + off)} {f2(y + off)} m {f2(x + door)} {f2(y + off)} l "
                       f"{f2(x + door + 36)} {f2(y + off)} m {f2(x + rw - off)} {f2(y + off)} l "
                       f"{f2(x + rw - off)} {f2(y + rh - off)} l {f2(x + off)} {f2(y + rh - off)} l "
                       f"{f2(x + off)} {f2(y + off)} l S")
                    seg += 5
                rr = 36
                cs(f"0.25 w {f2(x + door)} {f2(y + rr)} m {f2(x + door + rr * k)} {f2(y + rr)} "
                   f"{f2(x + door + rr)} {f2(y + rr * k)} {f2(x + door + rr)} {f2(y)} c S 0.5 w")
        cs("EMC")
        # HATCH layer: 45-degree hatching clipped to two of every three rooms
        cs("/OC /L2 BDC 0.35 G 0.1 w")
        for i in range(cols):
            for j in range(rows):
                if (i + j) % 3 == 1:
                    continue
                x, y = ox + i * rw + t, oy + j * rh + t
                w, h = rw - 2 * t, rh - 2 * t
                cs(f"q {f2(x)} {f2(y)} {f2(w)} {f2(h)} re W n")
                d = -h
                parts = []
                while d < w:
                    parts.append(f"{f2(x + d)} {f2(y)} m {f2(x + d + h)} {f2(y + h)} l")
                    d += 1.8
                    seg += 1
                cs(" ".join(parts) + " S Q")
        cs("EMC")
        # FURNITURE layer: small rectangles and circles, hairlines
        cs("/OC /L3 BDC 0.2 0.2 0.6 RG 0 w")
        for i in range(cols):
            for j in range(rows):
                x, y = ox + i * rw + 20, oy + j * rh + 20
                for _ in range(80):
                    fx, fy = x + r.uniform(0, rw - 60), y + r.uniform(0, rh - 60)
                    if r.random() < 0.5:
                        cs(f"{f2(fx)} {f2(fy)} {f2(r.uniform(6, 30))} {f2(r.uniform(6, 20))} re S")
                        seg += 4
                    else:
                        c = r.uniform(3, 9)
                        cs(f"{f2(fx + c)} {f2(fy)} m {f2(fx + c)} {f2(fy + c * k)} {f2(fx + c * k)} "
                           f"{f2(fy + c)} {f2(fx)} {f2(fy + c)} c {f2(fx - c * k)} {f2(fy + c)} "
                           f"{f2(fx - c)} {f2(fy + c * k)} {f2(fx - c)} {f2(fy)} c S")
        cs("EMC")
        # DIMENSIONS layer
        cs("/OC /L4 BDC 0.6 0 0 RG 0.15 w")
        for i in range(cols):
            x, y = ox + i * rw, oy - 30
            cs(f"{f2(x)} {f2(y)} m {f2(x + rw)} {f2(y)} l S {f2(x)} {f2(y - 6)} m {f2(x)} {f2(y + 6)} l S "
               f"{f2(x + rw)} {f2(y - 6)} m {f2(x + rw)} {f2(y + 6)} l S")
            cs(f"BT /F1 7 Tf 0.6 0 0 rg {f2(x + rw / 2 - 12)} {f2(y + 3)} Td ({rw * 25.4 / 72 * 100 / 1000:.2f} m) Tj ET")
            seg += 3
        for i in range(cols):
            for j in range(rows):
                x, y = ox + i * rw + 10, oy + j * rh + rh / 2
                cs(f"{f2(x)} {f2(y)} m {f2(x + rw - 20)} {f2(y)} l S")
                for xx in (x, x + rw - 20):
                    cs(f"{f2(xx)} {f2(y - 3)} m {f2(xx)} {f2(y + 3)} l S")
                seg += 3
        cs("EMC")
        # TEXT layer
        cs("/OC /L5 BDC 0 g")
        for i in range(cols):
            for j in range(rows):
                x, y = ox + i * rw + 14, oy + j * rh + rh - 30
                area = (rw * rh) * (25.4 / 72 * 100 / 1000) ** 2
                cs(f"BT /F2 9 Tf {f2(x)} {f2(y)} Td (ROOM {j + 1}{i + 1:02d}) Tj "
                   f"/F1 6 Tf 0 -9 Td ({area:.1f} m2) Tj ET")
        cs("EMC")
        res = {"Font": {"F1": f1, "F2": f2_},
               "Properties": {f"L{i}": ocgs[n] for i, n in enumerate(layer_names)}}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, cs.data(), res, [0, 0, W, H])
        refs = [ocgs[n] for n in layer_names]
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: A0 floor plan with layers",
                        id_seed="cad-floorplan",
                        catalog_extra={"OCProperties": {"OCGs": refs,
                                                        "D": {"Order": refs, "ON": refs,
                                                              "OFF": []}}})
    return {"pages": 1, "segments": seg}


def cad_schematic(out: Path, ctx: Ctx) -> dict:
    """A1 schematic: symbol Form XObjects instanced thousands of times."""
    r = rng("cad-schematic")
    W, H = A1_LAND
    seg = 0
    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        f1 = pdf.obj(std_font("Courier"))
        f2_ = pdf.obj(std_font("Helvetica-Bold"))

        def form(ops: str, bbox: list[float]) -> Ref:
            return pdf.stream(ops.encode("ascii"), {"Type": "XObject", "Subtype": "Form",
                                                    "BBox": bbox, "Resources": {}})

        sym = {
            "R": form("0.4 w 0 0 m 4 0 l 5 3 l 7 -3 l 9 3 l 11 -3 l 13 3 l 15 -3 l 16 0 l 20 0 l S",
                      [-1, -4, 21, 4]),
            "C": form("0.4 w 0 0 m 8 0 l S 12 0 m 20 0 l S 8 -5 m 8 5 l S 12 -5 m 12 5 l S",
                      [-1, -6, 21, 6]),
            "G": form("0.4 w 0 0 m 0 -5 l S -6 -5 m 6 -5 l S -4 -7 m 4 -7 l S -2 -9 m 2 -9 l S",
                      [-7, -10, 7, 1]),
            "D": form("0 0 m 0 1.4 -1.4 1.4 0 1.4 c 1.4 1.4 1.4 0 1.4 0 c f", [-2, -2, 2, 2]),
            "U": form("0.5 w 0 0 40 60 re S 0.3 w " + " ".join(
                f"-6 {6 + 7 * i} m 0 {6 + 7 * i} l 40 {6 + 7 * i} m 46 {6 + 7 * i} l" for i in range(7))
                + " S", [-7, -1, 47, 61]),
        }
        seg_per = {"R": 9, "C": 4, "G": 4, "D": 0, "U": 18}
        cs = Content()
        cs(f"0 G 2 w 20 20 {f2(W - 40)} {f2(H - 40)} re S")
        _title_block(cs, W, "SCHEMATIC - SHEET 1", "FX-A1-0002", "NTS")
        cs("0 G 0.3 w")
        cols, rows = 48, 30
        dx, dy = (W - 200) / cols, (H - 300) / rows
        for i in range(cols):
            for j in range(rows):
                x, y = 100 + i * dx, 220 + j * dy
                kind = r.choice("RRRCCGDU") if (i * rows + j) % 37 else "U"
                if kind == "U" and (i % 4 or j % 3):
                    kind = "R"
                rot = r.choice((0, 90))
                m = "1 0 0 1" if rot == 0 else "0 1 -1 0"
                cs(f"q {m} {f2(x)} {f2(y)} cm /{kind} Do Q")
                seg += seg_per[kind]
                # wires: orthogonal polyline to the next grid node
                cs(f"{f2(x + 20)} {f2(y)} m {f2(x + dx * 0.6)} {f2(y)} l "
                   f"{f2(x + dx * 0.6)} {f2(y + dy * 0.5)} l {f2(x + dx)} {f2(y + dy * 0.5)} l S")
                seg += 3
                if (i + j) % 5 == 0:
                    cs(f"BT /F1 4 Tf {f2(x + 2)} {f2(y + 5)} Td (N{i:02d}_{j:02d}) Tj ET")
        res = {"Font": {"F1": f1, "F2": f2_}, "XObject": sym}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, cs.data(), res, [0, 0, W, H])
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: A1 schematic",
                        id_seed="cad-schematic")
    return {"pages": 1, "segments": seg}


def cad_fem_mesh(out: Path, ctx: Ctx, nx: int = 200, ny: int = 140) -> dict:
    """A0 FEA-style plot: ~56k filled triangles plus hairline edges."""
    r = rng("cad-fem")
    W, H = A0_LAND
    ox, oy, sw, shh = 80.0, 200.0, W - 160, H - 280
    pts = [[(ox + sw * i / nx + (r.uniform(-0.3, 0.3) * sw / nx if 0 < i < nx else 0),
             oy + shh * j / ny + (r.uniform(-0.3, 0.3) * shh / ny if 0 < j < ny else 0))
            for j in range(ny + 1)] for i in range(nx + 1)]

    def field(x: float, y: float) -> float:
        u, v = (x - ox) / sw, (y - oy) / shh
        return 0.5 + 0.25 * math.sin(6 * u) * math.cos(4 * v) + 0.25 * math.exp(
            -((u - 0.7) ** 2 + (v - 0.4) ** 2) * 20)

    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        f1 = pdf.obj(std_font("Helvetica-Bold"))
        cs = Content()
        tris = 0
        for i in range(nx):
            for j in range(ny):
                a, b, c, d = pts[i][j], pts[i + 1][j], pts[i + 1][j + 1], pts[i][j + 1]
                for tri in ((a, b, c), (a, c, d)):
                    cx = sum(p[0] for p in tri) / 3
                    cy = sum(p[1] for p in tri) / 3
                    cr, cg, cb = jet(field(cx, cy))
                    cs(f"{cr:.3f} {cg:.3f} {cb:.3f} rg {f2(tri[0][0])} {f2(tri[0][1])} m "
                       f"{f2(tri[1][0])} {f2(tri[1][1])} l {f2(tri[2][0])} {f2(tri[2][1])} l f")
                    tris += 1
        cs("0 G 0 w")
        edges = 0
        for i in range(nx + 1):
            col = " ".join(f"{f2(x)} {f2(y)} l" for x, y in pts[i])
            cs(f"{f2(pts[i][0][0])} {f2(pts[i][0][1])} m {col} S")
            edges += ny
        for j in range(ny + 1):
            row = " ".join(f"{f2(pts[i][j][0])} {f2(pts[i][j][1])} l" for i in range(nx + 1))
            cs(f"{f2(pts[0][j][0])} {f2(pts[0][j][1])} m {row} S")
            edges += nx
        diag = []
        for i in range(nx):
            for j in range(ny):
                diag.append(f"{f2(pts[i][j][0])} {f2(pts[i][j][1])} m "
                            f"{f2(pts[i + 1][j + 1][0])} {f2(pts[i + 1][j + 1][1])} l")
                edges += 1
        cs(" ".join(diag) + " S")
        cs(f"BT /F1 28 Tf 0 g 80 120 Td (FEA mesh: {tris} filled triangles, {edges} hairline edges "
           f"\\(synthetic\\)) Tj ET")
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, cs.data(), {"Font": {"F1": f1}}, [0, 0, W, H])
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: A0 FEA mesh",
                        id_seed="cad-fem")
    return {"pages": 1, "segments": edges, "triangles": tris}


# --------------------------------------------------------------------------
# transparency
# --------------------------------------------------------------------------
def tr_alpha(out: Path, ctx: Ctx) -> dict:
    from reportlab.lib.colors import Color
    from reportlab.lib.utils import ImageReader
    from PIL import Image

    from .imaging import synth_photo

    r = rng("tr-alpha")
    c = rl_canvas(out, "FastPDF fixture: constant alpha, overlapping shapes")
    W, H = A4
    # Page 1: grid of overlapping translucent circles
    for i in range(9):
        for j in range(12):
            c.setFillColorRGB(*jet((i * 12 + j) / 108))
            c.setFillAlpha(0.1 + 0.8 * ((i + j) % 9) / 8)
            c.setStrokeColorRGB(0, 0, 0)
            c.setStrokeAlpha(0.5)
            c.circle(50 + i * 60, 60 + j * 62, 48, stroke=1, fill=1)
    c.showPage()
    # Page 2: 3000 random translucent rectangles
    for _ in range(3000):
        c.setFillColorRGB(r.random(), r.random(), r.random())
        c.setFillAlpha(r.uniform(0.05, 0.6))
        c.rect(r.uniform(-20, W), r.uniform(-20, H), r.uniform(10, 140), r.uniform(10, 140),
               stroke=0, fill=1)
    c.showPage()
    # Page 3: translucent text over a gradient, RGBA image with SMask
    c.saveState()
    p = c.beginPath()
    p.rect(0, 0, W, H)
    c.clipPath(p, stroke=0, fill=0)
    c.linearGradient(0, 0, W, H, [Color(0.1, 0.2, 0.6), Color(0.9, 0.6, 0.1)])
    c.restoreState()
    c.setFont("Helvetica-Bold", 44)
    for k in range(8):
        c.setFillColorRGB(1, 1, 1)
        c.setFillAlpha(0.15 + k * 0.1)
        c.drawString(40, H - 80 - k * 52, f"alpha {0.15 + k * 0.1:.2f} text")
    photo = synth_photo(r, 900, 600).convert("RGBA")
    alpha = Image.radial_gradient("L").resize((900, 600)).point(lambda v: 255 - v)
    photo.putalpha(alpha)
    c.setFillAlpha(1)
    c.drawImage(ImageReader(photo), 60, 60, width=480, height=320, mask="auto")
    c.showPage()
    c.save()
    return {"pages": 3}


BLEND_MODES = ("Normal", "Multiply", "Screen", "Overlay", "Darken", "Lighten", "ColorDodge",
               "ColorBurn", "HardLight", "SoftLight", "Difference", "Exclusion", "Hue",
               "Saturation", "Color", "Luminosity")


def tr_blend_modes(out: Path, ctx: Ctx) -> dict:
    from reportlab.lib.colors import Color

    c = rl_canvas(out, "FastPDF fixture: 16 blend modes")
    W, H = A4
    for k, mode in enumerate(BLEND_MODES):
        col, row = k % 4, k // 4
        x, y = 30 + col * 138, H - 210 - row * 190
        c.saveState()
        p = c.beginPath()
        p.rect(x, y, 130, 150)
        c.clipPath(p, stroke=0, fill=0)
        c.linearGradient(x, y, x + 130, y, [Color(0.1, 0.1, 0.1), Color(1, 0.9, 0.2),
                                            Color(0.2, 0.6, 1)])
        c.setBlendMode(mode)
        for (dx, dy), rgb in zip(((40, 95), (85, 95), (62, 55)),
                                 ((1, 0, 0), (0, 1, 0), (0, 0, 1))):
            c.setFillColorRGB(*rgb)
            c.circle(x + dx, y + dy, 38, stroke=0, fill=1)
        c.setBlendMode("Normal")
        c.restoreState()
        c.setFont("Helvetica-Bold", 10)
        c.drawString(x, y - 14, mode)
    c.showPage()
    c.save()
    return {"pages": 1}


def tr_softmask_groups(out: Path, ctx: Ctx) -> dict:
    """Soft masks (luminosity / alpha), isolated & knockout groups, page group."""
    W, H = A4
    k = 0.5523

    def circle(cx: float, cy: float, rad: float) -> str:
        return (f"{f2(cx + rad)} {f2(cy)} m {f2(cx + rad)} {f2(cy + rad * k)} {f2(cx + rad * k)} "
                f"{f2(cy + rad)} {f2(cx)} {f2(cy + rad)} c {f2(cx - rad * k)} {f2(cy + rad)} "
                f"{f2(cx - rad)} {f2(cy + rad * k)} {f2(cx - rad)} {f2(cy)} c {f2(cx - rad)} "
                f"{f2(cy - rad * k)} {f2(cx - rad * k)} {f2(cy - rad)} {f2(cx)} {f2(cy - rad)} c "
                f"{f2(cx + rad * k)} {f2(cy - rad)} {f2(cx + rad)} {f2(cy - rad * k)} {f2(cx + rad)} "
                f"{f2(cy)} c")

    with open(out, "wb") as fp:
        pdf = RawPdf(fp)
        font = pdf.obj(std_font("Helvetica-Bold"))
        radial = pdf.obj({"ShadingType": 3, "ColorSpace": "DeviceGray",
                          "Coords": [100, 100, 0, 100, 100, 100],
                          "Function": {"FunctionType": 2, "Domain": [0, 1], "C0": [1], "C1": [0],
                                       "N": 1}, "Extend": [True, True]})
        lum_group = pdf.stream(b"/Sh sh", {"Type": "XObject", "Subtype": "Form", "BBox": [0, 0, 200, 200],
                                           "Group": {"S": "Transparency", "CS": "DeviceGray"},
                                           "Resources": {"Shading": {"Sh": radial}}})
        alpha_group = pdf.stream(("0 g /GA gs " + circle(100, 100, 90) + " f").encode(),
                                 {"Type": "XObject", "Subtype": "Form", "BBox": [0, 0, 200, 200],
                                  "Group": {"S": "Transparency"},
                                  "Resources": {"ExtGState": {"GA": {"ca": 0.4}}}})
        gs = {
            "SL": pdf.obj({"Type": "ExtGState", "SMask": {"Type": "Mask", "S": "Luminosity",
                                                          "G": lum_group, "BC": [0]}}),
            "SA": pdf.obj({"Type": "ExtGState", "SMask": {"Type": "Mask", "S": "Alpha",
                                                          "G": alpha_group}}),
            "S0": pdf.obj({"Type": "ExtGState", "SMask": "None"}),
            "A5": pdf.obj({"Type": "ExtGState", "ca": 0.5, "CA": 0.5}),
            "A7": pdf.obj({"Type": "ExtGState", "ca": 0.7}),
        }
        groups = {}
        body = ("/A7 gs 1 0 0 rg " + circle(70, 90, 55) + " f 0 0.7 0 rg " + circle(130, 90, 55)
                + " f 0 0 1 rg " + circle(100, 140, 55) + " f").encode()
        for iso in (False, True):
            for ko in (False, True):
                name = f"G{int(iso)}{int(ko)}"
                groups[name] = pdf.stream(body, {"Type": "XObject", "Subtype": "Form",
                                                 "BBox": [0, 0, 200, 200],
                                                 "Group": {"S": "Transparency", "I": iso, "K": ko},
                                                 "Resources": {"ExtGState": {"A7": gs["A7"]}}})
        inner = pdf.stream(("/A5 gs 0.9 0.5 0 rg " + circle(100, 100, 70) + " f").encode(),
                           {"Type": "XObject", "Subtype": "Form", "BBox": [0, 0, 200, 200],
                            "Group": {"S": "Transparency", "I": True},
                            "Resources": {"ExtGState": {"A5": gs["A5"]}}})
        outer = pdf.stream(b"/A5 gs 0 0.4 0.8 rg 0 0 200 120 re f /In Do",
                           {"Type": "XObject", "Subtype": "Form", "BBox": [0, 0, 200, 200],
                            "Group": {"S": "Transparency"},
                            "Resources": {"ExtGState": {"A5": gs["A5"]}, "XObject": {"In": inner}}})
        groups["N"] = outer
        cs = Content()
        cs("0.9 g 0 0 595.28 841.89 re f")
        for i in range(0, 600, 30):
            cs(f"0.6 g {i} 0 15 842 re f")
        cs("q 1 0 0 1 40 600 cm /SL gs 0.8 0 0.2 rg 0 0 200 200 re f Q")
        cs("q 1 0 0 1 300 600 cm /SA gs 0 0.3 0.8 rg 0 0 200 200 re f Q /S0 gs")
        cs("BT /F1 10 Tf 0 g 40 588 Td (Luminosity soft mask \\(radial\\)) Tj 260 0 Td "
           "(Alpha soft mask \\(group ca 0.4\\)) Tj ET")
        for n_, (name, label) in enumerate((("G00", "non-isolated, non-knockout"),
                                            ("G01", "knockout"), ("G10", "isolated"),
                                            ("G11", "isolated + knockout"))):
            x, y = 40 + (n_ % 2) * 260, 340 - (n_ // 2) * 230
            cs(f"q 1 0 0 1 {x} {y} cm /{name} Do Q")
            cs(f"BT /F1 10 Tf {x} {y - 10} Td ({label}) Tj ET")
        cs("q 0.5 0 0 0.5 470 60 cm /N Do Q")
        cs("BT /F1 8 Tf 430 50 Td (nested groups with ca 0.5) Tj ET")
        res = {"Font": {"F1": font}, "ExtGState": gs, "XObject": groups}
        pages_ref = pdf.alloc()
        kids: list[Ref] = []
        add_page(pdf, pages_ref, kids, cs.data(), res,
                 extra={"Group": {"Type": "Group", "S": "Transparency", "CS": "DeviceRGB",
                                  "I": True}})
        simple_document(pdf, kids, pages_ref, title="FastPDF fixture: soft masks and groups",
                        id_seed="tr-softmask")
    return {"pages": 1}


def jobs() -> list[Job]:
    return [
        Job("vector-heavy/bezier-curves-40k.pdf", vec_bezier, "open_ok",
            "4 頁、每頁 10,000 條獨立 stroke 的 cubic bezier（各自顏色／線寬）。",
            pages=4, features=("bezier", "many-strokes"), cost=2),
        Job("vector-heavy/fills-gradients-strokes.pdf", vec_fills_gradients, "open_ok",
            "2000 個自交多邊形（nonzero／even-odd 交替）、clip 內的 axial／radial 漸層、line cap／join／dash／miter limit。",
            pages=3, features=("fill-rules", "clip", "axial", "radial", "dash"),
            producer="reportlab", cost=1.5),
        Job("vector-heavy/shading-types-patterns.pdf", vec_shadings_patterns, "open_ok",
            "Shading type 1–7（含 Type 4 PostScript calculator 函數、Type 3 stitching、triangle／lattice mesh、Coons／tensor patch）與 tiling／shading pattern。",
            pages=1, features=("shading-1-7", "function-type-4", "tiling-pattern",
                               "shading-pattern"), cost=0.3),
        Job("vector-heavy/dense-polyline-map-300k.pdf", vec_polyline_map, "open_ok",
            "A3 單頁地圖風格：300 條、每條 1000 點的 polyline（約 30 萬線段）＋虛線格線。",
            pages=1, features=("polyline", "dense-path", "dash"), cost=2),
        Job("cad/a0-floorplan-layers.pdf", cad_floorplan, "open_ok",
            "A0 橫式平面圖：牆線、45° hatch、家具、尺寸標註、文字，含 6 個 OCG 圖層（/OC BDC…EMC），數萬條細線與 0 寬 hairline。",
            pages=1, features=("A0", "OCG", "hairline", "hatching", "clip"), cost=2),
        Job("cad/a1-schematic-xobjects.pdf", cad_schematic, "open_ok",
            "A1 橫式電路圖：符號以 Form XObject 定義並重複引用約 1,400 次，加上正交走線與 4pt 標籤。",
            pages=1, features=("A1", "form-xobject-reuse", "tiny-text"), cost=0.6),
        Job("cad/a0-fem-mesh-hairlines.pdf", cad_fem_mesh, "open_ok",
            "A0 有限元素網格：約 56,000 個填色三角形＋約 8.4 萬條 0 寬 hairline 邊。",
            pages=1, features=("A0", "hairline", "many-fills"), cost=3),
        Job("transparency/alpha-overlap.pdf", tr_alpha, "open_ok",
            "常數 alpha（ca／CA）：重疊半透明圓形、3000 個半透明矩形、半透明文字疊在漸層上、RGBA 影像（SMask）。",
            pages=3, features=("ca", "CA", "SMask", "alpha-text"), producer="reportlab", cost=2),
        Job("transparency/blend-modes-16.pdf", tr_blend_modes, "open_ok",
            "全部 16 種 blend mode（/BM），RGB 圓形疊在漸層上。",
            pages=1, features=("blend-modes",), producer="reportlab", cost=0.2),
        Job("transparency/softmask-groups.pdf", tr_softmask_groups, "open_ok",
            "Luminosity／Alpha soft mask（ExtGState /SMask）、isolated／knockout transparency group 四種組合、巢狀 group、page-level /Group。",
            pages=1, features=("SMask-luminosity", "SMask-alpha", "isolated", "knockout",
                               "page-group"), cost=0.2),
    ]
