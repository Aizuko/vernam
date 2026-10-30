# Makes the app icon's sizes, for crates/app/assets/app-icon, from artwork
# drawn as a rounded square (crates/app/assets/app-icon/source.png):
#   uv run --with pillow --with numpy scripts/app-icon.py SOURCE.png OUT_DIR
# then oxipng -o max --strip safe OUT_DIR/[0-9]*.png.
#
# An .icns draws its own shape. Apple's grid: on a 1024 canvas, an 824
# square at 100 with continuous corners (a superellipse here) and a soft
# shadow below. The artwork's own square is cut out, scaled to 824, and
# given that shape, so its margins, corners and background don't matter.
# Artwork without transparency counts the white around its square as
# background (only white reached from the edges: white inside stays).
import math
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

source, out_dir = Path(sys.argv[1]), Path(sys.argv[2])
CANVAS, BODY, INSET = 1024, 824, 100
SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024]


def squircle(size: int, exponent: float = 5.0, supersample: int = 4) -> Image.Image:
    """A superellipse mask, |x|^n + |y|^n <= 1, antialiased by drawing big."""
    big = size * supersample
    mask = Image.new("L", (big, big), 0)
    points = []
    steps = 4096
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        x = abs(c) ** (2 / exponent) * (1 if c >= 0 else -1)
        y = abs(s) ** (2 / exponent) * (1 if s >= 0 else -1)
        points.append(((x + 1) * big / 2, (y + 1) * big / 2))
    ImageDraw.Draw(mask).polygon(points, fill=255)
    return mask.resize((size, size), Image.LANCZOS)


def reached_from_edges(candidates: np.ndarray) -> np.ndarray:
    """Which of `candidates` (a bool mask) connect to the image's border."""
    # A copy: an image made from an array doesn't keep the fill's writes.
    mask = Image.fromarray(np.uint8(candidates * 255), "L").copy()
    w, h = mask.size
    for x, y in [(x, 0) for x in range(w)] + [(x, h - 1) for x in range(w)] + [(0, y) for y in range(h)] + [(w - 1, y) for y in range(h)]:
        if mask.getpixel((x, y)) == 255:
            ImageDraw.floodfill(mask, (x, y), 128)
    return np.asarray(mask) == 128


def shaved(image: Image.Image, rim: int = 6) -> Image.Image:
    """`image` with the `rim` pixels along its outer edge made transparent,
    for `filled` to replace: artwork edges carry a light halo that would
    show as a line inside the shape. Holes in the artwork stay as drawn.
    On the whole canvas, before cropping: the crop ends at the halo, which
    then has no transparent neighbor."""
    alpha = np.asarray(image.getchannel("A"))
    outside = Image.fromarray(np.uint8(reached_from_edges(alpha <= 200) * 255), "L")
    rim_or_outside = np.asarray(outside.filter(ImageFilter.MaxFilter(2 * rim + 1))) == 255
    image = image.copy()
    image.putalpha(Image.fromarray(np.where(rim_or_outside, 0, alpha).astype(np.uint8), "L"))
    return image


def filled(image: Image.Image) -> Image.Image:
    """`image` opaque: where it's transparent (outside its own corners), the
    colors nearest, spread by ever wider blurs of the premultiplied image."""
    rgba = np.asarray(image, dtype=np.float64) / 255
    alpha = rgba[..., 3:]
    color = rgba[..., :3] * alpha
    out_color, out_alpha = color.copy(), alpha.copy()
    for radius in (4, 8, 16, 32, 64, 128):
        blur = lambda channels: np.asarray(
            Image.fromarray(np.uint8(np.clip(channels, 0, 1) * 255)).filter(ImageFilter.GaussianBlur(radius)),
            dtype=np.float64,
        ) / 255
        spread_color = np.dstack([blur(color[..., i]) for i in range(3)])
        spread_alpha = blur(alpha[..., 0])[..., None]
        # Over what's there, under what's drawn.
        out_color = out_color + spread_color * (1 - out_alpha)
        out_alpha = out_alpha + spread_alpha * (1 - out_alpha)
    rgb = out_color / np.maximum(out_alpha, 1e-6)
    return Image.fromarray(np.uint8(np.clip(rgb, 0, 1) * 255 + 0.5), "RGB").convert("RGBA")


def white_surround_cleared(image: Image.Image, tolerance: int = 40) -> Image.Image:
    """`image` with the near-white reached from its edges made transparent,
    when it has no transparency of its own."""
    if image.getchannel("A").getextrema()[0] < 255:
        return image
    rgb = np.asarray(image.convert("RGB"), dtype=np.int16)
    background = reached_from_edges(np.abs(rgb - 255).max(axis=2) <= tolerance)
    out = np.asarray(image).copy()
    out[..., 3] = np.where(background, 0, 255)
    return Image.fromarray(out, "RGBA")


art = white_surround_cleared(Image.open(source).convert("RGBA"))
# The artwork's square: what's nearly opaque. Fainter pixels around it
# (antialiasing, stray haze) are left out.
square = art.getchannel("A").point(lambda a: 255 if a > 200 else 0).getbbox()
body = filled(shaved(art).crop(square)).resize((BODY, BODY), Image.LANCZOS)
shape = squircle(BODY)
body.putalpha(shape)

icon = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
# A soft shadow, a little below, as macOS icons have.
shadow = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
shadow_mask = Image.new("L", (CANVAS, CANVAS), 0)
shadow_mask.paste(shape, (INSET, INSET + 12))
shadow.putalpha(shadow_mask.point(lambda a: a * 0.35).filter(ImageFilter.GaussianBlur(14)))
icon.alpha_composite(shadow)
icon.alpha_composite(body, (INSET, INSET))

out_dir.mkdir(parents=True, exist_ok=True)
for size in SIZES:
    icon.resize((size, size), Image.LANCZOS).save(out_dir / f"{size}x{size}.png")
