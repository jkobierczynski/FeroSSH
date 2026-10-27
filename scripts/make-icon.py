# Regenerates crates/app/ferossh.ico from the shapes in docs/logo.svg,
# drawn with Pillow at 4x and scaled down:
#     python3 scripts/make-icon.py crates/app
import sys
from PIL import Image, ImageDraw
out = sys.argv[1]
F = [(242,242),(782,242),(650,374),(374,374),(374,446),(579,446),(447,578),(374,578),(374,650),(242,782)]
A, B = (0xEB,0x50,0x16), (0xF7,0x92,0x1C)
def render(size):
    # Small icons draw the F larger so it stays legible.
    z = 1.3 if size <= 48 else 1.0
    S = size*4; k = S/1024
    img = Image.new('RGBA',(S,S),(0,0,0,0))
    tile = Image.new('L',(S,S),0)
    ImageDraw.Draw(tile).rounded_rectangle([0,0,S-1,S-1], radius=112*k, fill=255)
    img.paste((0x25,0x25,0x25,255),(0,0),tile)
    grad = Image.new('RGBA',(S,S))
    gd = ImageDraw.Draw(grad)
    for x in range(S):
        t = min(max(((x/k-512)/z+270)/540,0),1)
        gd.line([(x,0),(x,S)], fill=tuple(round(a+(b-a)*t) for a,b in zip(A,B))+(255,))
    m = Image.new('L',(S,S),0)
    ImageDraw.Draw(m).polygon([(((x-512)*z+512)*k,((y-512)*z+512)*k) for x,y in F], fill=255)
    img.paste(grad,(0,0),m)
    return img.resize((size,size), Image.LANCZOS)
sizes=[256,64,48,40,32,24,20,16]
icons=[render(s) for s in sizes]
icons[0].save(f'{out}/ferossh.ico', format='ICO', sizes=[(s,s) for s in sizes], append_images=icons[1:])
