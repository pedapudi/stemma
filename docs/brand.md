# stemma — brand

## the name

In textual criticism a *stemma codicum* is the tree of surviving manuscript
witnesses — each one a corrupt, divergent copy — reconstructed to show how they
all descend from a single lost archetype. The philologist works backward from
the variants to the thing they are all versions of.

That is the product. Many surface forms, one referent.

**stemma** is the ecosystem: the resolution engine, the console, the evals, the
protocol. **stemmadb** is the storage layer specifically — the sidecar
`.stemmadb` file and the crate that owns it (`crates/stemmadb`). Use the
narrower name only when the storage layer is what you mean; when in doubt the
project is stemma.

Both are **always lowercase** — in prose, in headings, in the UI, at the start
of a sentence. Never `Stemma`, never `STEMMA`, never `StemmaDB`. If a sentence
reads badly starting with a lowercase name, rewrite the sentence.

## the mark

A stemma codicum read as a plant: witness branches fanning out from one stem
like the veins of a leaf, converging on the archetype above them.

```
                  ●             archetype  ·  the lost original, the one
                                referent. filled, and the ONLY colored
                  ·             element in the entire system
               ╱  │  ╲          apex (12, 8)  ·  every branch converges here
             ╱    │    ╲
            │     │     │       witness branches  ·  three surviving copies.
            │  ╲  │  ╱  │       they leave the apex diagonally and straighten
            │     │     │       to vertical at the baseline — a leaf's veins,
            ┴─────┴─────┴       not a bracket
            4     12    20
                                contamination  ·  two short ticks where a
            baseline y = 20     reading crossed between witnesses
```

Read bottom-up: the surviving forms stand at the baseline, contaminated by each
other, and resolve upward to the one thing they are copies of. The dot's center
sits 2.4 units above the apex, close enough that its lower edge laps the stroke
cap — mark and dot are one gesture, not a diagram with a bullet over it.

### construction

| | |
|---|---|
| grid | `viewBox="0 0 24 24"` |
| stroke | `1.6`, `stroke-linecap="round"`, `stroke-linejoin="round"`, `fill="none"` |
| stem | `M12 20 C12 15 12 12 12 8` |
| left witness | `M4 20 C4 13 9 12 12 8` |
| right witness | `M20 20 C20 13 15 12 12 8` |
| contamination | `M7.5 15.5 L9.2 17.2` · `M16.5 15.5 L14.8 17.2` |
| archetype dot | `cx 12 · cy 5.6 · r 2.1` |

The branches are the same curve mirrored: the first control point sits directly
above the base (vertical departure), the second sits 62% of the way toward the
stem at two-thirds height. That is what makes them read as veins rather than as
a bracket or a chevron. Don't redraw them by eye — copy the paths.

The contamination ticks always slope **toward the stem as they descend** (left
tick down-and-right, right tick down-and-left). Reversed, they read as arrows
leaving the tree instead of readings crossing between witnesses.

The canonical asset is [`assets/brand/mark.svg`](../assets/brand/mark.svg); the
console carries the same geometry inline in `ui/static/index.html`.

## color

One rule, and it is the whole system:

> **strokes are `currentColor`. the dot is the only colored element.**

The mark carries no ink color of its own — it strokes `currentColor` and so
renders dark-on-light and light-on-dark automatically, following whatever text
color surrounds it. Across all sixteen console themes the mark never needs
retinting.

The dot fills `var(--brand-accent, …)`, and that variable takes exactly two
values:

| ground | `--brand-accent` |
|---|---|
| light | `#2FA46A` |
| dark | `#3FB87A` |

The dark value is a step up in lightness so the dot holds contrast against a
dark paper; it is the same green, not a second one. `ui/static/ui.css` sets
`--brand-accent` per theme block, which is the only place either value should
appear.

There is no second accent, no gradient, no fill anywhere except that circle.
The mark's own `--accent` (the UI's functional blue) is a different token for a
different job and never touches the brand.

### the `color=""` fallback

Every shipped SVG carries a `color` presentation attribute on its root — dark
ink on the light assets, light ink on the dark ones. That is a **fallback**,
the exact analogue of the `#2FA46A` inside `var(--brand-accent, #2FA46A)`: it
exists so the file still renders sensibly when loaded through `<img>`, where
there is no host text color to inherit. A page that inlines the mark should
delete the attribute, or override it with one line:

```css
.brand svg { color: inherit; }   /* stroke follows the surrounding ink */
```

## the wordmark

`stemmadb` (or `stemma`) set in **JetBrains Mono 700**, lowercase, tracked
`+0.01em`:

```css
--brand-mono: "JetBrains Mono", ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace;
```

The wordmark is **pinned** to that stack. The console lets the user pick a
typeface for everything else; the wordmark does not follow the picker. A brand
that changes shape with a preference is not a brand.

A surface qualifier — `console`, `server`, `eval` — may sit after the wordmark
in the same mono at ~9.5px, uppercase, tracked `+0.16em`, in the functional
accent. That is the one place the brand name and an uppercase treatment appear
together, and the uppercase applies to the qualifier only, never to `stemmadb`.

The horizontal lockup is [`assets/brand/lockup.svg`](../assets/brand/lockup.svg):
mark, a 9-unit gap, wordmark. Nine units is three-eighths of the mark's width —
the same rhythm as the console topbar. Don't re-space it.

## clear space & minimum size

- **clear space** — one dot-diameter (4.2 mark units, ≈18% of the grid) on
  every side. Nothing crowds the baseline or the dot.
- **the mark** reads down to **20px**. Below that the contamination ticks fuse
  into the branches — swap to the favicon, don't shrink the mark.
- **the favicon** ([`assets/brand/favicon.svg`](../assets/brand/favicon.svg))
  reads down to **16px**. It is the same grammar simplified for the size: ticks
  dropped, stroke thickened `1.6 → 2.6`, branches pulled wider, apex lowered to
  `y=9`, dot enlarged to `r=3` so it stays a disc and not a smudge. It keeps
  its accent dot at every size — the dot is the point of the mark.
- **the lockup** holds to **~120px** wide — below that its mark falls under the
  20px floor. Narrower than that, use the mark alone.
- **the display variant**
  ([`assets/brand/leaf-large.svg`](../assets/brand/leaf-large.svg)) is for docs
  headers and covers at **≥160px**: the same grammar elaborated to seven
  witnesses, four contamination ticks, hairline `1.2` strokes, and still
  exactly one dot. Its branch bases step up toward the outside so the lower
  silhouette closes like a leaf.

## assets

| file | what | ink | accent |
|---|---|---|---|
| [`mark.svg`](../assets/brand/mark.svg) | the canonical mark | `currentColor`, fallback `#15181C` | `#2FA46A` |
| [`mark-dark.svg`](../assets/brand/mark-dark.svg) | the dark-ground twin | `currentColor`, fallback `#EDEFEA` | `#3FB87A` |
| [`favicon.svg`](../assets/brand/favicon.svg) | the 16px form | media-query light/dark | follows the ground |
| [`lockup.svg`](../assets/brand/lockup.svg) | mark + `stemmadb` | `currentColor`, fallback `#15181C` | `#2FA46A` |
| [`leaf-large.svg`](../assets/brand/leaf-large.svg) | display / hero | `currentColor`, fallback `#15181C` | `#2FA46A` |

**In a web page** — inline the SVG (an `<img>` cannot inherit `currentColor`),
drop the `color` attribute, and let the theme supply `--brand-accent`.

**In a markdown README** — GitHub strips CSS, so use the fixed pair:

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/mark-dark.svg">
  <img alt="stemma" src="assets/brand/mark.svg" width="48">
</picture>
```

## do / don't

**do**

- let the strokes inherit `currentColor` — the mark is theme-adaptive by
  construction and needs no per-theme variant
- keep the accent as the single dot; override `--brand-accent` and nothing else
- swap to the favicon below 20px rather than shrinking the mark
- copy the paths; the geometry is the mark

**don't**

- don't recolor the dot away from green, or move it off the stem axis
- don't add a second color, a gradient, a fill, or a stroke color to any branch
- don't rotate, flip, shear, or stretch non-uniformly — the mark grows upward
  and the fan is symmetric about `x=12`; a mirrored or tilted stemma is a
  different diagram
- don't set the mark on a photograph, a texture, or any busy ground; it is a
  hairline drawing and needs flat paper
- don't outline it, box it, add a drop shadow, or animate it
- don't uppercase the name, title-case it, or write `StemmaDB`
- don't put the wordmark in the picker font, or in a face that isn't the pinned
  mono

## voice

Lowercase, declarative, measured — the same register as the rest of the house.

- **lowercase leaning.** headings, labels, buttons, nav, log lines. Sentence
  case in body prose; proper nouns and identifiers keep their own casing
  (`SQLite`, `FTS5`, `BM25`, `Resolve`).
- **declarative, not promotional.** "resolves mentions to rows" — not
  "intelligently understands your data". The product's promise is *evidence*,
  and the copy should never claim more than the evidence panel shows.
- **claims carry a number.** a score, a candidate count, a latency, a corpus
  size. If there is no number, say less.
- **`·` separates peers** in a metadata run: `4 candidates · 0.82 · fts5+vec`.
  Not a pipe, not a bullet, not a comma.
- **`→` means navigation or derivation**: `query → mentions → candidates →
  resolution`. Not for "results in" in prose.
- **honest empty states.** name what is absent and the next move — "no
  candidates above 0.3 · widen the threshold or index more of the corpus" — not
  "nothing to see here". Never fill an empty state with an illustration.
- **honest incompleteness.** what is planned reads as planned. The console
  ships a research-preview tag beside the wordmark for exactly this reason; it
  is informational and never interactive.
- **no exclamation marks, no first person for the system.** the system reports;
  it does not celebrate. "indexed 12,480 rows" — not "all done!"


## the wordmark

machines write `stemmadb` — code, paths, identifiers, shell examples, package
names. display contexts write **stemma·db**: the interpunct is the house
separator carrying the archetype dot, the wordmark's one colored element
(#2FA46A on light grounds, #3FB87A on dark). JetBrains Mono 700, lowercase,
always. the ecosystem name alone is `stemma`, plain.

never: uppercase any form · color any glyph but the interpunct · use the
interpunct form in code · set the wordmark in a non-mono face.

## the asset inventory

| asset | light | dark | use |
|---|---|---|---|
| mark | `mark.svg` | `mark-dark.svg` | the 24×24 glyph alone |
| wordmark | `wordmark.svg` | `wordmark-dark.svg` | text-only placements |
| lockup | `lockup.svg` | `lockup-dark.svg` | mark + wordmark, horizontal |
| stacked lockup | `lockup-stacked.svg` | `lockup-stacked-dark.svg` | square-ish placements |
| hero leaf | `leaf-large.svg` | (currentColor) | docs headers, banners |
| favicon | `favicon.svg` | (self-grounded) | browser tabs |
| app icon | `icon.svg` | `icon-dark.svg` | 512 tile, maskable-safe 60% zone |
| banner | `banner.svg` | `banner-dark.svg` | 1200×630 — readme, social cards |
| png renders | `png/icon-{16..512}.png`, `png/apple-touch-icon.png`, `png/banner*.png` | | hosts that can't svg |

svg ink is `currentColor` with a `color=""` fallback for `<img>` use; the
light/dark twins differ only in that fallback and the accent value. regenerate
pngs with cairosvg (`svg2png`) — no other tooling assumed.
