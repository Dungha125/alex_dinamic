"""Generate Alex logo assets (SVG + PNG + ICO + raw RGBA) from polygon geometry."""
import sys, os
from PIL import Image, ImageDraw, ImageFilter

OUT = sys.argv[1] if len(sys.argv) > 1 else "assets"
os.makedirs(OUT, exist_ok=True)

C1, C2, C3 = (0, 229, 255), (41, 121, 255), (170, 0, 255)   # cyan -> blue -> violet
BG = (8, 12, 26)

# --- glyphs on a 100-high grid (x offset, polygons) ---
A = [[(0,100),(37,0),(63,0),(100,100),(74,100),(50,32),(26,100)],
     [(38,70),(62,70),(66,84),(34,84)]]
L = [[(0,0),(22,0),(22,78),(72,78),(72,100),(10,100),(0,90)]]
E = [[(0,10),(10,0),(72,0),(72,22),(22,22),(22,78),(72,78),(72,100),(10,100),(0,90)],
     [(30,39),(64,39),(64,61),(30,61)]]
X = [[(0,0),(27,0),(84,100),(57,100)], [(57,0),(84,0),(27,100),(0,100)]]
WORD = [(A,0),(L,116),(E,204),(X,292)]
WORD_W = 376

# icon mark: hexagon + chevron-A
def hexagon(cx, cy, r):
    import math
    return [(cx + r*math.cos(math.radians(a)), cy + r*math.sin(math.radians(a))) for a in range(-90, 270, 60)]
MARK = [[(18,82),(42,18),(58,18),(82,82),(66,82),(50,38),(34,82)],
        [(41,62),(59,62),(62,72),(38,72)]]

def svg_poly(polys, ox=0, oy=0, s=1.0, fill="url(#g)"):
    out = []
    for p in polys:
        pts = " ".join(f"{ox+x*s:.1f},{oy+y*s:.1f}" for x,y in p)
        out.append(f'<polygon points="{pts}" fill="{fill}"/>')
    return "\n  ".join(out)

grad = f'''<linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="rgb{C1}"/><stop offset=".5" stop-color="rgb{C2}"/><stop offset="1" stop-color="rgb{C3}"/>
  </linearGradient>
  <filter id="glow" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="3" result="b"/>
    <feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>'''

hexpts = " ".join(f"{x:.1f},{y:.1f}" for x,y in hexagon(50,50,48))
hexin = " ".join(f"{x:.1f},{y:.1f}" for x,y in hexagon(50,50,44))
icon_svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="512" height="512">
  <defs>{grad}</defs>
  <polygon points="{hexpts}" fill="url(#g)"/>
  <polygon points="{hexin}" fill="rgb{BG}"/>
  <g filter="url(#glow)">{svg_poly(MARK)}</g>
</svg>'''

letters = "\n  ".join(svg_poly(g, ox=150+x*0.9, oy=25, s=0.9) for g,x in WORD)
logo_svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 520 140" width="1040" height="280">
  <defs>{grad}</defs>
  <g transform="translate(10,10) scale(1.2)">
  <polygon points="{hexpts}" fill="url(#g)"/>
  <polygon points="{hexin}" fill="rgb{BG}"/>
  <g filter="url(#glow)">{svg_poly(MARK)}</g>
  </g>
  <g filter="url(#glow)">{letters}</g>
</svg>'''
open(f"{OUT}/icon.svg","w").write(icon_svg)
open(f"{OUT}/logo.svg","w").write(logo_svg)

# --- raster ---
def gradient(w, h):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            t = (x/w + y/h) / 2
            a, b, u = (C1, C2, t*2) if t < .5 else (C2, C3, (t-.5)*2)
            px[x,y] = tuple(int(a[i]+(b[i]-a[i])*u) for i in range(3))
    return img

def render_icon(size, ss=4):
    S = size*ss
    k = S/100
    img = Image.new("RGBA", (S,S), (0,0,0,0))
    g = gradient(S,S).convert("RGBA")
    m = Image.new("L",(S,S),0); d = ImageDraw.Draw(m)
    d.polygon([(x*k,y*k) for x,y in hexagon(50,50,48)], fill=255)
    img.paste(g,(0,0),m)
    m2 = Image.new("L",(S,S),0); ImageDraw.Draw(m2).polygon([(x*k,y*k) for x,y in hexagon(50,50,44)], fill=255)
    img.paste(Image.new("RGBA",(S,S),BG+(255,)),(0,0),m2)
    mm = Image.new("L",(S,S),0); dd = ImageDraw.Draw(mm)
    for p in MARK: dd.polygon([(x*k,y*k) for x,y in p], fill=255)
    glow = mm.filter(ImageFilter.GaussianBlur(S/40)).point(lambda v: int(v*.7))
    img.paste(Image.new("RGBA",(S,S),C1+(255,)),(0,0),glow)
    img.paste(g,(0,0),mm)
    return img.resize((size,size), Image.LANCZOS)

big = render_icon(512); big.save(f"{OUT}/icon.png")
big.save(f"{OUT}/alex.ico", sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
open(f"{OUT}/icon32.rgba","wb").write(render_icon(32).tobytes())
open(f"{OUT}/icon64.rgba","wb").write(render_icon(64).tobytes())

# wordmark png for README
W,H,ss = 1040,280,2
S=(W*ss,H*ss); k=2*ss
logo = Image.new("RGBA",S,(0,0,0,0))
ic = render_icon(int(120*k/ ss)*ss//ss*1)  # icon ~240px
logo.paste(render_icon(240*ss//1//1 if False else 240).resize((240*ss,240*ss)),(20*ss,20*ss))
mm = Image.new("L",S,0); dd = ImageDraw.Draw(mm)
for g_,x0 in WORD:
    for p in g_: dd.polygon([((150+x0*0.9+x*0.9)*k,(25+y*0.9)*k) for x,y in p], fill=255)
gl = mm.filter(ImageFilter.GaussianBlur(8*ss)).point(lambda v:int(v*.6))
logo.paste(Image.new("RGBA",S,C1+(255,)),(0,0),gl)
logo.paste(gradient(*S).convert("RGBA"),(0,0),mm)
logo.resize((W,H),Image.LANCZOS).save(f"{OUT}/logo.png")
print("ok")
