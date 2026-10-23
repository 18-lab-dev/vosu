---
type: Guide
title: "vosu branding"
description: "The vosu mark, 18-LAB lockup, colours, files, and how the rasters are rendered."
---

# vosu branding

vosu is an 18-LAB product (stage code 18-01). Its art follows the [18-LAB design system](tools/design.md) and sits beside vosulay's and the first-party tools' art as one family. The terms are in the "Branding assets" section of [`LICENSE`](../LICENSE).

## Mark

<img src="../assets/branding/vosu-mark.svg" alt="" width="96">

osu!'s hit circle in ink with the product's part in accent in front of it; vosulay uses the same rule with an overlay panel. For vosu the part is a V standing before the circle: v + osu.

- Drawn on the 18-LAB mark's 120-unit grid: circle centre (70, 70), radius 28, 12-unit stroke; V with horizontal top terminals from x 16 to 62, y 16 to 60, 13.5-unit arms and a 6-unit foot. The circle is cut 6 units around the V. Outer extents are x and y 16–104.
- The mark is never recoloured beyond the two roles below, outlined, rotated, given effects, or separated into V and circle. No ppy or osu! logos.
- 16 and 24 px have their own pixel-fitted masters (`vosu-app-icon-16.svg`, `vosu-app-icon-24.svg`) with 2- and 3-pixel strokes on whole pixels. Use them for tray, favicon and small taskbar icons; everything from 32 px up scales the master.

| Role | Light (DRAFTING) | Dark (WORKSTATION) |
| --- | --- | --- |
| Circle, wordmark | `#132030` | `#ebe8e1` |
| V (accent) | `#175fad` | `#ff963f` |
| Rule, endorsement | `#a7b2bd`, `#3b4858` | `#3b3f45`, `#b8b5ad` |

The app icon is the mark at 0.8 scale on a graphite tile with 12 % corner radius. The dashboard and report pages draw the mark inline with `currentColor` and `--accent`, so it follows the page's mode.

## Lockup

`vosu-logo.svg` (light surfaces) and `vosu-logo-on-dark.svg`: mark, the lowercase wordmark in Barlow Condensed 600 (x-height 46 units, baseline 90), a hairline rule, and the 18-LAB logotype as endorsement in one ink colour (cap height 20). The 18-LAB logotype's mark stands in for "18"; never set a typed "18" beside it. Keep 16 units of clear space around the lockup. Use it at 64 px height or more, where the endorsement stays legible; smaller, use the mark alone. The dashboard header is the same lockup in live type.

## Files

| File | Use |
| --- | --- |
| `vosu-app-icon.svg` | Master app icon; Linux `hicolor/scalable` |
| `vosu-app-icon-16.svg`, `vosu-app-icon-24.svg` | Pixel-fitted small masters |
| `vosu-mark.svg` | Mark without tile, follows `prefers-color-scheme` |
| `vosu-logo.svg`, `vosu-logo-on-dark.svg` | Lockup |
| `vosu-banner.svg`, `vosu-banner-light.svg` | README header, 1280×320, chosen with `<picture>` |
| `vosu-social-preview.svg` / `.png` | GitHub repository social preview, 1280×640; upload the PNG in the repository settings |
| `icons/vosu-<size>.png` | 16–512 px: Linux tray pixmaps (16–64) and `hicolor` package icons |
| `../icon.ico` | Windows executable, taskbar and tray icon: 16, 20, 24, 32, 40, 48, 64, 128 (DIB) and 256 (PNG) |
| `../server/favicon.ico` | Dashboard favicon: 16, 32, 48 |

## Rendering

The SVG files are the source; every raster is generated and committed, so builds and packages need no image tools. After changing a master:

```sh
python scripts/render-brand.py           # needs resvg on PATH (0.48)
python scripts/render-brand.py --check   # committed rasters match the masters
python scripts/check-asset-hashes.py --write
```

Text in the logos, banners and social preview is outlined, so they render the same on GitHub and in image viewers without fonts. It was set in Barlow Condensed 600, Barlow 400 and IBM Plex Mono 500 from `assets/server/` (HarfBuzz shaping, glyph outlines via fontTools). To change a line, outline the new text from the same fonts, keep the sizes and positions, and re-render.
