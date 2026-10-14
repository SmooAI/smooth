# Generates images/th-ring-icon.svg: the th mark inside the Smoo AI social avatar's gradient band
# (teal -> coral -> orange -> teal, sampled from smooai apps/brand social-avatar@512px.png).
# Usage: python3 scripts/gen-th-ring-icon.py
import math
import re
from pathlib import Path

IMAGES = Path(__file__).resolve().parent.parent / "images"
TH = re.search(r'<path[^>]* d="([^"]+)"', (IMAGES / "smooth-icon.svg").read_text()).group(1)
# Band stops by math angle (deg, CCW from +x), sampled from the Smoo AI social avatar.
STOPS = [(-60, (0x00,0xa6,0xa6)), (80, (0xff,0x6b,0x6c)), (180, (0xf4,0x9f,0x0a)), (300, (0x00,0xa6,0xa6))]
def color(a):
    a = (a + 60) % 360 - 60
    for (a0,c0),(a1,c1) in zip(STOPS, STOPS[1:]):
        if a0 <= a <= a1:
            t=(a-a0)/(a1-a0); return '#%02x%02x%02x' % tuple(round(x+(y-x)*t) for x,y in zip(c0,c1))
def mark(size=128, ring=6.0, gap=4.0, bg='#0f1527', outer_bg=None, glyph_grad=('#19c2c2','#4f7bff'), n=120):
    c=size/2; r=c-ring/2-1
    out=[f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" width="{size}" height="{size}">',
         '<defs><linearGradient id="th" x1="0" y1="0" x2="1" y2="1">'
         f'<stop offset=".35" stop-color="{glyph_grad[0]}"/><stop offset="1" stop-color="{glyph_grad[1]}"/></linearGradient></defs>']
    if outer_bg: out.append(f'<rect width="{size}" height="{size}" fill="{outer_bg}"/>')
    out.append(f'<circle cx="{c}" cy="{c}" r="{c-1}" fill="{bg}"/>')
    step=360/n
    for i in range(n):
        a0=i*step; a1=a0+step+0.6  # overlap hides seams
        x0,y0=c+r*math.cos(math.radians(a0)), c-r*math.sin(math.radians(a0))
        x1,y1=c+r*math.cos(math.radians(a1)), c-r*math.sin(math.radians(a1))
        out.append(f'<path d="M{x0:.3f},{y0:.3f}A{r},{r} 0 0 0 {x1:.3f},{y1:.3f}" fill="none" stroke="{color(a0+step/2)}" stroke-width="{ring}"/>')
    # th glyph box in its 150 viewBox: x 19.24..129.6, y 36.39..113.6 → centre in the inner disc.
    inner=r-ring/2-gap; w=129.6-19.24; h=113.6-36.39
    s=(inner*1.32)/w
    tx=c-(19.24+w/2)*s; ty=c-(36.39+h/2)*s
    out.append(f'<path transform="translate({tx:.3f},{ty:.3f}) scale({s:.4f})" fill="url(#th)" d="{TH}"/>')
    out.append('</svg>')
    return '\n'.join(out)
if __name__ == "__main__":
    (IMAGES / "th-ring-icon.svg").write_text(mark())
