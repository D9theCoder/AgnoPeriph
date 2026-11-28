"""Cuts the static Saira and Figtree files bundled here from the Google Fonts variable fonts.

gpui always asks DirectWrite for the normal width, so every width the design needs is its own
family name.

    python -I cut_fonts.py <Saira[wdth,wght].ttf> <Figtree[wght].ttf>

Needs fontTools.
"""

import pathlib
import sys

from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

OUT = pathlib.Path(__file__).resolve().parent
NAME_IDS = (1, 2, 3, 4, 6, 16, 17, 21, 22, 25)

# (file, source, axes, family, style, weight class)
CUTS = [
    ("saira-medium.ttf", "saira", {"wght": 500, "wdth": 100}, "Saira", "Medium", 500),
    ("saira-semibold.ttf", "saira", {"wght": 600, "wdth": 100}, "Saira", "SemiBold", 600),
    ("saira-bold.ttf", "saira", {"wght": 700, "wdth": 100}, "Saira", "Bold", 700),
    # Keycap legends and values: font-stretch 92%.
    ("saira-keys.ttf", "saira", {"wght": 600, "wdth": 92}, "Saira Keys", "Regular", 600),
    ("figtree-regular.ttf", "figtree", {"wght": 400}, "Figtree", "Regular", 400),
    ("figtree-medium.ttf", "figtree", {"wght": 500}, "Figtree", "Medium", 500),
    ("figtree-semibold.ttf", "figtree", {"wght": 600}, "Figtree", "SemiBold", 600),
    ("figtree-bold.ttf", "figtree", {"wght": 700}, "Figtree", "Bold", 700),
]


def rename(font, family, style, weight):
    name = font["name"]
    name.names = [record for record in name.names if record.nameID not in NAME_IDS]
    full = family if style == "Regular" else f"{family} {style}"
    is_ribbi = style in ("Regular", "Bold")
    values = {
        1: family if is_ribbi else full,
        2: style if is_ribbi else "Regular",
        3: f"AgnoPeriph: {full}",
        4: full,
        6: f"{family.replace(' ', '')}-{style}",
    }
    if not is_ribbi:
        values[16] = family
        values[17] = style
    for name_id, value in values.items():
        name.setName(value, name_id, 3, 1, 0x409)

    os2 = font["OS/2"]
    os2.usWeightClass = weight
    os2.usWidthClass = 5
    bold = style == "Bold"
    os2.fsSelection = (os2.fsSelection & ~0b1100001) | (0x20 if bold else 0)
    if style == "Regular":
        os2.fsSelection |= 0x40
    font["head"].macStyle = 1 if bold else 0


def main(saira, figtree):
    sources = {"saira": saira, "figtree": figtree}
    for file, source, axes, family, style, weight in CUTS:
        font = instantiateVariableFont(TTFont(sources[source]), axes)
        if "STAT" in font:
            del font["STAT"]
        rename(font, family, style, weight)
        font.save(OUT / file)
        print(f"{file}: {(OUT / file).stat().st_size} bytes")


if __name__ == "__main__":
    main(*sys.argv[1:3])
