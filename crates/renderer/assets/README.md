# Bundled piece sets

Each directory holds the twelve SVG pieces (`wK.svg` … `bP.svg`) of one set,
embedded at compile time with `include_str!` and rasterized by `resvg` into the
same 4×3 atlas.

| Set | Source |
|---|---|
| `cburnett` | https://github.com/lichess-org/lila/tree/master/public/piece/cburnett |
| `merida` | https://github.com/lichess-org/lila/tree/master/public/piece/merida |
| `alpha` | https://github.com/lichess-org/lila/tree/master/public/piece/alpha |
| `leipzig` | https://github.com/lichess-org/lila/tree/master/public/piece/leipzig |

All four sets are distributed by the Lichess project (`lila`, GPL-3.0-or-later)
alongside its GPL-compatible artwork, which is why they are bundled here under
this repository's GPL-3.0-or-later licence.

Select the set in `~/.config/lichess-rs/config.toml`:

```toml
[theme]
pieces = "merida"
```
