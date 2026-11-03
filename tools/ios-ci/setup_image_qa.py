"""Pixel contract for the portrait UIKit setup view, not the former PC picker."""
from PIL import Image

def assess(path):
    with Image.open(path) as image:
        width,height=image.size
        if not (height>width and 0.55<width/height<0.9):
            return {'accepted':False,'reason':'Expected the portrait iPad test fixture'}
        pixels=image.convert('RGB').resize((328,472))
        samples=list(pixels.getdata());count=len(samples)
        background=sum(r<18 and 7<g<35 and 18<b<60 and b>g>r for r,g,b in samples)/count
        def bright(p):return min(p)>120 and max(p)-min(p)<45
        bright_fraction=sum(bright(p) for p in samples)/count
        # Four separate pieces of instructions must appear in their expected
        # central bands. Solid blue, white/black frames and a lone cursor fail.
        bands=[]
        for low,high in [(0.38,0.44),(0.44,0.49),(0.49,0.57),(0.57,0.64)]:
            selected=[pixels.getpixel((x,y)) for y in range(int(low*472),int(high*472)) for x in range(5,323)]
            bands.append(sum(bright(p) for p in selected)/len(selected))
        accepted=background>0.85 and 0.005<bright_fraction<0.08 and all(n>0.01 for n in bands)
        return {'accepted':accepted,'contract':'portrait UIKit dark-blue setup with four white text blocks','background_fraction':background,'white_text_fraction':bright_fraction,'text_band_fractions':bands,'full_game_acceptance':False}
