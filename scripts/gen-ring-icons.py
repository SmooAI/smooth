# Generates the ringed brand marks: each product mark inside the Smoo AI social avatar's gradient
# band (teal -> coral -> orange -> teal, sampled from smooai apps/brand social-avatar@512px.png),
# so Smooth, Big Smooth and SmoothFlow read as Smoo AI products.
#   images/th-ring-icon.svg          th (Smooth / Big Smooth / the Chrome extension)
#   images/smoothflow-ring-icon.svg  th over the three SmoothFlow streams
# Usage: python3 scripts/gen-ring-icons.py [--out DIR]   SVG masters only (also importable: mark(), tile())
#        python3 scripts/gen-ring-icons.py --assets        also re-render every app icon in this repo
#                                                          (needs rsvg-convert and, for .icns, macOS iconutil)
import argparse
import math
import re
import struct
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
IMAGES = ROOT / "images"
TH = re.search(r'<path[^>]* d="([^"]+)"', (IMAGES / "smooth-icon.svg").read_text()).group(1)
GROUND = "#0f1527"
# Band stops by math angle (deg, CCW from +x), sampled from the Smoo AI social avatar.
STOPS = [(-60, (0x00, 0xA6, 0xA6)), (80, (0xFF, 0x6B, 0x6C)), (180, (0xF4, 0x9F, 0x0A)), (300, (0x00, 0xA6, 0xA6))]
# SmoothFlow's streams, in its 256-unit icon-source.svg coordinates (gold, orange, coral).
STREAMS = [(150, "#f49f0a"), (176, "#fb7a4d"), (202, "#ff6b6c")]


def color(a):
    a = (a + 60) % 360 - 60
    for (a0, c0), (a1, c1) in zip(STOPS, STOPS[1:]):
        if a0 <= a <= a1:
            t = (a - a0) / (a1 - a0)
            return "#%02x%02x%02x" % tuple(round(x + (y - x) * t) for x, y in zip(c0, c1))


def th_glyph(cx, cy, width, fill="url(#th)"):
    """The th path, scaled to `width` and centred on (cx, cy). Glyph box in its 150 viewBox: x 19.24..129.6, y 36.39..113.6."""
    w, h = 129.6 - 19.24, 113.6 - 36.39
    s = width / w
    tx, ty = cx - (19.24 + w / 2) * s, cy - (36.39 + h / 2) * s
    return f'<path transform="translate({tx:.3f},{ty:.3f}) scale({s:.4f})" fill="{fill}" d="{TH}"/>'


def ring(cx, cy, r, width, n=120):
    step = 360 / n
    out = []
    for i in range(n):
        a0 = i * step
        a1 = a0 + step + 0.6  # overlap hides seams
        x0, y0 = cx + r * math.cos(math.radians(a0)), cy - r * math.sin(math.radians(a0))
        x1, y1 = cx + r * math.cos(math.radians(a1)), cy - r * math.sin(math.radians(a1))
        out.append(f'<path d="M{x0:.3f},{y0:.3f}A{r:.3f},{r:.3f} 0 0 0 {x1:.3f},{y1:.3f}" fill="none" stroke="{color(a0 + step / 2)}" stroke-width="{width:.3f}"/>')
    return out


def body(kind, cx, cy, inner):
    """What sits inside the band: the th alone, or (smoothflow) the th riding the three streams."""
    if kind == "th":
        return [th_glyph(cx, cy, inner * 1.32)]
    # SmoothFlow: map its 256-unit art onto the inner disc's square, clipped to the disc.
    s = inner * 2 / 256
    ox, oy = cx - inner, cy - inner
    out = [f'<clipPath id="disc"><circle cx="{cx}" cy="{cy}" r="{inner:.3f}"/></clipPath>', '<g clip-path="url(#disc)">']
    for y, c in STREAMS:
        y += 16  # sit the streams a touch lower so the glyph clears them inside a circle
        out.append(
            f'<path transform="translate({ox:.3f},{oy:.3f}) scale({s:.4f})" d="M-10 {y} C 60 {y - 30}, 120 {y + 40}, 270 {y}" '
            f'fill="none" stroke="{c}" stroke-width="12" stroke-linecap="round"/>'
        )
    out.append("</g>")
    out.append(th_glyph(cx, cy - inner * 0.2, inner * 1.12))
    return out


def mark(size=128, ring_w=None, gap=None, kind="th", n=120, ground=GROUND, backdrop=None, pad=0.0):
    """A round mark on a transparent (or `backdrop`) square; `pad` is the margin around the disc, as a fraction of size."""
    disc = size * (1 - 2 * pad)
    # Below ~40px a proportional band blurs into the ground, so it gets a floor in pixels.
    ring_w = ring_w if ring_w is not None else max(disc * 6 / 128, 1.6)
    gap = gap if gap is not None else max(disc * 4 / 128, 0.6)
    n = n if disc >= 40 else 60
    c = size / 2
    outer = c * (1 - 2 * pad) - max(1, size / 128)
    r = outer - ring_w / 2
    inner = r - ring_w / 2 - gap
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" width="{size}" height="{size}">',
        '<defs><linearGradient id="th" x1="0" y1="0" x2="1" y2="1"><stop offset=".35" stop-color="#19c2c2"/><stop offset="1" stop-color="#4f7bff"/></linearGradient></defs>',
    ]
    if backdrop:
        out.append(f'<rect width="{size}" height="{size}" fill="{backdrop}"/>')
    out.append(f'<circle cx="{c}" cy="{c}" r="{outer:.3f}" fill="{ground}"/>')
    out += ring(c, c, r, ring_w, n)
    out += body(kind, c, c, inner)
    out.append("</svg>")
    return "\n".join(out)


def tile(size=1024, kind="th"):
    """Full-bleed square for platforms that mask the icon themselves (iOS, Android legacy): ground fills the square, ring inset."""
    return mark(size, kind=kind, backdrop=GROUND, pad=0.1)  # disc = the 80% maskable safe zone


def png(svg, size, out):
    with tempfile.NamedTemporaryFile("w", suffix=".svg") as f:
        f.write(svg)
        f.flush()
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), f.name, "-o", str(out)], check=True)


def ico(pngs, out):
    """A .ico holding PNG entries (Vista+ format; every browser reads it)."""
    blobs = [(s, Path(p).read_bytes()) for s, p in pngs]
    head = struct.pack("<HHH", 0, 1, len(blobs))
    offset = 6 + 16 * len(blobs)
    dirs, data = b"", b""
    for s, b in blobs:
        dirs += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(b), offset + len(data))
        data += b
    Path(out).write_bytes(head + dirs + data)


# macOS icon grid: the artwork is 824 of 1024, centred.
MAC_PAD = 100 / 1024


def icns(kind, out):
    with tempfile.TemporaryDirectory(suffix=".iconset") as d:
        for base in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = base * scale
                name = f"icon_{base}x{base}{'@2x' if scale == 2 else ''}.png"
                png(mark(px, kind=kind, pad=MAC_PAD), px, Path(d) / name)
        subprocess.run(["iconutil", "-c", "icns", d, "-o", str(out)], check=True)


def assets():
    web = ROOT / "crates/smooth-web/web/public"
    ext = ROOT / "crates/smooth-cli/src/chrome/extension"
    flow = ROOT / "apps/smoothflow/Resources"
    # Big Smooth: macOS app, Electron, the web SPA (favicons round; touch/PWA icons full-bleed, the OS masks them).
    icns("th", ROOT / "scripts/macos/BigSmooth.icns")
    png(mark(512, pad=MAC_PAD), 512, ROOT / "desktop/assets/icon.png")
    (web / "smooth-icon.svg").write_text(mark() + "\n")
    for s in (16, 32):
        png(mark(s), s, web / f"favicon-{s}x{s}.png")
    png(mark(512), 512, web / "favicon.png")
    ico([(16, web / "favicon-16x16.png"), (32, web / "favicon-32x32.png")], web / "favicon.ico")
    for s, name in ((180, "apple-touch-icon.png"), (167, "apple-touch-icon-167.png"), (152, "apple-touch-icon-152.png"), (120, "apple-touch-icon-120.png")):
        png(tile(s), s, web / name)
    for s in (192, 512):
        png(tile(s), s, web / f"pwa-{s}x{s}.png")
    # The Chrome extension.
    (ext / "icon.svg").write_text(mark() + "\n")
    for s in (16, 32, 48, 128):
        png(mark(s), s, ext / f"icons/{s}.png")
    # SmoothFlow (macOS).
    icns("smoothflow", flow / "SmoothFlow.icns")
    (flow / "icon-source.svg").write_text(mark(256, kind="smoothflow") + "\n")
    (flow / "macos-1024.svg").write_text(mark(1024, kind="smoothflow", pad=MAC_PAD) + "\n")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=IMAGES)
    ap.add_argument("--assets", action="store_true", help="also re-render every app icon in the repo")
    a = ap.parse_args()
    (a.out / "th-ring-icon.svg").write_text(mark() + "\n")
    (a.out / "smoothflow-ring-icon.svg").write_text(mark(kind="smoothflow") + "\n")
    if a.assets:
        assets()
