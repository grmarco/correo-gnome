#!/usr/bin/env python3
"""Regenera el icono de Correo GNOME a partir de un color base.

Uso:
    scripts/recolor-logo.py                      # verde por defecto (#2b7a52)
    scripts/recolor-logo.py --base '#9141ac'     # violeta original aproximado
    scripts/recolor-logo.py --base '#2b7a52' --preview

El resto de tonos se derivan en OKLCH (luminosidad L, croma C, tono h) con las
mismas reglas que se aplicaron a mano al diseñar el icono. Solo usa la
biblioteca estándar; --preview necesita `inkscape` en el PATH.
"""

import argparse
import math
import shutil
import subprocess
import sys
from pathlib import Path

DEFAULT_BASE = "#2b7a52"
ROOT = Path(__file__).resolve().parent.parent
ICON = ROOT / "data/icons/hicolor/scalable/apps/io.github.gmarco.Correo.svg"
PREVIEW_DIR = ROOT / "docs/logo-preview"
PREVIEW_SIZES = (512, 128, 64, 32)

# --- Conversión sRGB <-> OKLCH -------------------------------------------------


def _to_linear(c):
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _from_linear(c):
    return 12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055


def hex_to_oklch(value):
    value = value.strip().lstrip("#")
    if len(value) == 3:
        value = "".join(ch * 2 for ch in value)
    if len(value) != 6:
        raise ValueError(f"color hex no válido: #{value}")
    r, g, b = (_to_linear(int(value[i : i + 2], 16) / 255) for i in (0, 2, 4))
    l_ = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b) ** (1 / 3)
    m_ = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b) ** (1 / 3)
    s_ = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b) ** (1 / 3)
    L = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_
    a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_
    bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_
    return L, math.hypot(a, bb), math.degrees(math.atan2(bb, a)) % 360


def _oklch_to_rgb(L, C, h):
    a, b = C * math.cos(math.radians(h)), C * math.sin(math.radians(h))
    l_ = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3
    m_ = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3
    s_ = (L - 0.0894841775 * a - 1.2914855480 * b) ** 3
    r = 4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_
    g = -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_
    bl = -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_
    return r, g, bl


def oklch_to_hex(L, C, h):
    """Convierte a hex reduciendo el croma hasta entrar en la gama sRGB."""
    L = min(max(L, 0.0), 1.0)
    for _ in range(40):
        rgb = _oklch_to_rgb(L, C, h)
        if all(-1e-4 <= c <= 1 + 1e-4 for c in rgb):
            break
        C *= 0.93
    return "#" + "".join(
        f"{round(min(max(_from_linear(min(max(c, 0.0), 1.0)), 0.0), 1.0) * 255):02x}" for c in rgb
    )


# --- Reglas de derivación ------------------------------------------------------


def derive_palette(base):
    L, C, h = hex_to_oklch(base)
    return {
        # Bolsillo frontal: degradado de un tono más claro (arriba) al base (abajo).
        "frente_alto": oklch_to_hex(L + 0.10, C * 1.05, h - 3),
        "frente_bajo": oklch_to_hex(L, C, h),
        # Borde claro de la solapa: bastante más luminoso y menos saturado.
        "brillo": oklch_to_hex(L + 0.22, C * 0.85, h - 5),
        # Franja base (grosor) y parte alta del interior: más oscuro.
        "franja": oklch_to_hex(L - 0.12, C * 0.8, h + 2),
        # Fondo del interior: casi neutro, oscuro, con un matiz del tono base.
        "interior": oklch_to_hex(0.30, min(C * 0.25, 0.03), h),
        # Sombra de la solapa sobre la carta.
        "sombra": oklch_to_hex(0.22, min(C * 0.2, 0.03), h),
        # Línea de «asunto» en la carta.
        "asunto": oklch_to_hex(L + 0.10, C * 1.05, h - 3),
    }


TEMPLATE = """<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <!-- Generado por scripts/recolor-logo.py (base {frente_bajo}). Licencia CC0-1.0. -->
  <defs>
    <linearGradient id="papel" x1="0" y1="16" x2="0" y2="84" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#ffffff"/>
      <stop offset="1" stop-color="#f6f5f4"/>
    </linearGradient>
    <linearGradient id="fondo-sobre" x1="0" y1="40" x2="0" y2="84" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="{franja}"/>
      <stop offset="1" stop-color="{interior}"/>
    </linearGradient>
    <linearGradient id="frente" x1="0" y1="44" x2="0" y2="104" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="{frente_alto}"/>
      <stop offset="1" stop-color="{frente_bajo}"/>
    </linearGradient>
    <clipPath id="silueta">
      <rect x="8" y="40" width="112" height="64" rx="12"/>
    </clipPath>
  </defs>
  <rect id="grosor" x="8" y="44" width="112" height="68" rx="12" fill="{franja}"/>
  <rect id="interior" x="8" y="40" width="112" height="64" rx="12" fill="url(#fondo-sobre)"/>
  <g id="carta" transform="rotate(-6 64 56)">
    <rect x="25" y="14" width="78" height="72" rx="6" fill="url(#papel)"/>
    <rect x="35" y="26" width="34" height="6" rx="3" fill="{asunto}"/>
    <rect x="35" y="39" width="56" height="4" rx="2" fill="#deddda"/>
    <rect x="35" y="48" width="46" height="4" rx="2" fill="#deddda"/>
  </g>
  <g clip-path="url(#silueta)">
    <path id="sombra-solapa" d="M8 42 C27 56 45 78 70 78 C94 78 109 60 120 40" fill="none" stroke="{sombra}" stroke-width="6" opacity="0.1"/>
    <path id="bolsillo" d="M8 46 C27 60 45 82 70 82 C94 82 109 64 120 44 V112 H8 Z" fill="url(#frente)"/>
    <path id="brillo" d="M8 46 C27 60 45 82 70 82 C94 82 109 64 120 44" fill="none" stroke="{brillo}" stroke-width="2"/>
  </g>
</svg>
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default=DEFAULT_BASE, help=f"color base en hex (por defecto {DEFAULT_BASE})")
    parser.add_argument("--output", type=Path, default=ICON, help="SVG de salida")
    parser.add_argument("--preview", action="store_true", help="regenerar docs/logo-preview/*.png con inkscape")
    args = parser.parse_args()

    try:
        palette = derive_palette(args.base)
    except ValueError as err:
        sys.exit(str(err))
    args.output.write_text(TEMPLATE.format(**palette), encoding="utf-8")
    print(f"{args.output}")
    for name, value in palette.items():
        print(f"  {name:12} {value}")
    # El simbólico es monocromo (#2e3436) y no depende del color base.

    if args.preview:
        inkscape = shutil.which("inkscape")
        if not inkscape:
            sys.exit("--preview necesita inkscape en el PATH")
        PREVIEW_DIR.mkdir(parents=True, exist_ok=True)
        for size in PREVIEW_SIZES:
            out = PREVIEW_DIR / f"io.github.gmarco.Correo-{size}.png"
            subprocess.run(
                [inkscape, str(args.output), "-w", str(size), "-h", str(size), "-o", str(out)],
                check=True,
                stderr=subprocess.DEVNULL,
            )
            print(f"{out}")


if __name__ == "__main__":
    main()
