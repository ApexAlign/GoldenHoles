# Custom characters: `shinobi-character/1.1`

You give an AI chatbot a photo (or a description). It answers with ONE JSON object. You save it as
`characters/<name>.json` next to the game and run with `--character characters/<name>.json`
(or `/character <file>` in game). Everyone in the match then sees your ninja styled like you.

Only the JSON ever reaches the game. The photo stays in the chatbot you chose. The game never trusts the JSON: every
field is validated and clamped on load and again by the server (unknown fields are ignored, bad values fall back to the
default), so a hand-edited or malicious file cannot break a match.

v1.1 adds ears, expressions, more visors, horns, nose, tail and shoulder width, so ogres, animals and robots fit.
Every v1 file (`"schema": "shinobi-character/1"`) keeps working and looks exactly as before.

### Tolerant loading

The schema string is a courtesy, not a security check. A file whose `schema` starts with `shinobi-character/` loads
(`1`, `1.1`; a newer number loads as 1.1 with a warning). A file with any other schema text (a chatbot may write
`ogre-character/1`) also loads, with a warning, as long as it is a JSON object. Inside the file:

- `null` counts as "left out" (no warning).
- An unknown field is ignored and reported: `ignored head.gills: unknown field`.
- An unknown value that contains a known word is read as that value (`"big_cat_eyes"` as `cat`, `"goofy_smile"` as
  `smile`, `"big_shrimp"` ears as `big`) and reported; anything else gets the default and a report.
- The game prints these lines at startup (stderr) or in the console after `/character`.

Why JSON and not a 3D model: it is small (41 bytes on the wire), chatbots produce it reliably from an image,
it always fits our rig and animations, and it keeps characters readable at distance (the game enforces a minimum glow).

## The format

All colours are `"#RRGGBB"` strings. Numbers outside a range are clamped. Every field is optional.

| Field | Type / range | Meaning |
|---|---|---|
| `schema` | `"shinobi-character/1.1"` | required text. Write exactly `shinobi-character/1.1` (`/1` also works) |
| `name` | string, max 16 chars `A-Z a-z 0-9 space _ -` | display name suggestion |
| `build.height` | 0.90 to 1.10 | body height scale (hitbox does not change) |
| `build.bulk` | 0.90 to 1.15 | body width scale (hitbox does not change) |
| `build.shoulders` | 0.90 to 1.20 | v1.1: shoulder plate and arm spacing (1.0 = stock) |
| `colors.suit_base` | colour | main outfit. Kept dark by the game (value is capped at 0.35) |
| `colors.suit_trim` | colour | cuffs, belt, panel edges |
| `colors.glow` | colour | the neon accent: circuit lines, visor rim, sword edge. The game lifts it to at least 60% brightness so you stay visible |
| `colors.visor` | colour | glowing eye slit / visor band |
| `colors.glove` | colour | hands, also shown in first person |
| `colors.scarf` | colour | scarf / tails |
| `colors.hair` | colour | hair, topknot or ponytail |
| `colors.skin` | colour | visible skin (face opening, hands if no glove) |
| `head.hood` | `"full"` `"open"` `"none"` | `open` shows hair and face, `none` is bare head |
| `head.mask` | `"none"` `"lower"` `"full"` `"oni"` `"fox"` | face covering |
| `head.hair_style` | `"none"` `"topknot"` `"ponytail"` `"spikes"` `"long"` | needs `hood` of `open` or `none` to be visible |
| `head.headband` | true / false | cloth headband |
| `head.visor_style` | `"slit"` `"dual"` `"band"` `"cat"` `"round"` `"angry"` | shape of the glowing eyes (v1.1 added `cat` slanted eyes with a dark pupil, `round` big square eyes, `angry` tilted slits) |
| `head.ears` | `"none"` `"pointy"` `"big"` `"long"` `"round"` `"cat"` | v1.1: ears in the skin colour that sway when idle. `pointy` elf/fox, `big` ogre flaps, `long` rabbit, `round` bear/mouse |
| `head.expression` | `"neutral"` `"smile"` `"grin"` `"angry"` `"surprised"` `"sleepy"` | v1.1: lit face marks (mouth, brows). Shown only when the hood is `open` or `none`, or the mask is `none` or `lower`. `neutral` draws nothing |
| `head.horns` | `"none"` `"small"` `"oni"` `"antlers"` | v1.1: horns on the head (separate from the `horns` accessory) |
| `head.nose` | `"none"` `"big"` `"snout"` | v1.1: a big nose, or an animal snout |
| `torso.plates` | `"none"` `"light"` `"heavy"` | chest and shoulder armour |
| `torso.sash` | true / false | waist sash |
| `torso.back_item` | `"sheath"` `"banner"` `"pack"` `"none"` | what is on the back |
| `torso.emblem` | integer 0 to 15 | chest glyph from the game's glyph sheet (0 = none) |
| `torso.tail` | `"none"` `"fox"` `"cat"` `"lizard"` | v1.1: a tail that sways and lifts while running (`fox`/`cat` use `colors.hair`, `lizard` uses `colors.skin`) |
| `scarf.style` | `"none"` `"short"` `"long"` `"tails"` | scarf shape |
| `scarf.length` | 0.0 to 1.0 | scarf length |
| `pattern.type` | `"lines"` `"hex"` `"stripes"` `"none"` | circuit pattern on the suit |
| `pattern.density` | 0.0 to 1.0 | how busy the pattern is |
| `weapons.edge` | colour | katana edge glow (falls back to `glow`) |
| `weapons.star` | colour | throwing-star glyph colour (falls back to `glow`) |
| `accessories` | up to 3 of `"shoulder_fins"` `"kunai_belt"` `"horns"` `"tabi_glow"` `"earring"` `"glasses"` | extras |

Default when a field is missing: the standard game ninja. Ears, expression, horns, nose and tail are only drawn near
you (like the hood and hair, they hide beyond 22 m); the hot core, outline and beacon colours are untouched.

### Examples

`characters/example_kitsune.json` (shown below), `characters/example_ogre.json` (green skin, big ears, goofy grin, cat
eyes, small horns, big nose) and `characters/example_fox.json` (pointy ears, snout, fox tail, angry visor).

```json
{
  "schema": "shinobi-character/1.1",
  "name": "Kitsune",
  "build": { "height": 1.02, "bulk": 0.95 },
  "colors": {
    "suit_base": "#16181f", "suit_trim": "#3a2f55", "glow": "#ff4fd8", "visor": "#7ff0ff",
    "glove": "#1c1c24", "scarf": "#c0173a", "hair": "#2b1a12", "skin": "#e0b494"
  },
  "head": { "hood": "open", "mask": "lower", "hair_style": "ponytail", "headband": true, "visor_style": "slit" },
  "torso": { "plates": "light", "sash": true, "back_item": "sheath", "emblem": 5 },
  "scarf": { "style": "tails", "length": 0.8 },
  "pattern": { "type": "lines", "density": 0.5 },
  "weapons": { "edge": "#ff4fd8", "star": "#7ff0ff" },
  "accessories": ["kunai_belt", "tabi_glow"]
}
```

## Prompt to give the chatbot

Attach one clear photo (face and upper body, good light). Paste everything inside the box as your message. Any chatbot
that accepts images will do.

```
You are a character designer for a Tron-meets-shinobi first-person game. Look at the attached photo and design a
ninja avatar that looks like this person, in the game's style: a dark stealth suit with neon glowing accents.

Output ONLY one JSON object. No explanation, no code fences, no comments, nothing before or after it.

Rules:
- "schema" MUST be exactly "shinobi-character/1.1". Never invent another schema name.
- Output ONLY the JSON object. No text before or after, no code fences, no comments.
- null is NOT allowed anywhere. If you do not know a value, leave that field out.
- Use ONLY the listed values below; do not invent new field names or new values.
- All colours are "#RRGGBB" strings.
- Allowed fields and values (anything else is ignored):
  name: up to 16 characters, letters digits space _ -
  build: height 0.90-1.10, bulk 0.90-1.15, shoulders 0.90-1.20
  colors: suit_base, suit_trim, glow, visor, glove, scarf, hair, skin
  head: hood ("full","open","none"), mask ("none","lower","full","oni","fox"),
        hair_style ("none","topknot","ponytail","spikes","long"), headband (true/false),
        visor_style ("slit","dual","band","cat","round","angry"),
        ears ("none","pointy","big","long","round","cat"),
        expression ("neutral","smile","grin","angry","surprised","sleepy"),
        horns ("none","small","oni","antlers"), nose ("none","big","snout")
  torso: plates ("none","light","heavy"), sash (true/false), back_item ("sheath","banner","pack","none"),
         emblem (integer 0-15), tail ("none","fox","cat","lizard")
  scarf: style ("none","short","long","tails"), length 0.0-1.0
  pattern: type ("lines","hex","stripes","none"), density 0.0-1.0
  weapons: edge (colour), star (colour)
  accessories: up to 3 of "shoulder_fins","kunai_belt","horns","tabi_glow","earring","glasses"
- Map the photo like this:
  * hair colour and style -> colors.hair and head.hair_style (bald or covered -> "none"; use hood "open" or "none" so hair shows)
  * skin tone -> colors.skin
  * the person's favourite or most visible clothing colour -> colors.glow (make it a bright, saturated neon version) and colors.scarf
  * glasses -> accessories "glasses"; earring -> "earring"; beard or lower-face covering -> head.mask "lower"
  * build and height impression -> build.height and build.bulk
  * the face: head.expression from the person's usual look (smile, grin, ...); visor_style from the eye shape
  * NON-HUMAN OR CARTOON CHARACTERS (ogre, animal, robot, monster): keep the same ninja suit, and carry the
    character in these fields only:
      - skin colour -> colors.skin (green for an ogre, orange for a fox, grey metal for a robot) and set head.hood
        to "open" or "none" and head.mask to "none" or "lower" so the face shows
      - ears -> head.ears (big flaps "big", fox "pointy", rabbit "long", bear "round", cat "cat")
      - horns or antlers -> head.horns; big nose or animal snout -> head.nose ("big" or "snout")
      - tail -> torso.tail ("fox","cat","lizard"); wide or huge body -> build.bulk and build.shoulders near the maximum
      - eyes -> head.visor_style ("cat" for cat eyes, "round" for cartoon eyes, "angry" for a scowl) and colors.visor
      - mood -> head.expression
    Do not add any new field for these.
  * the suit itself should stay dark: colors.suit_base is a very dark shade, suit_trim a muted accent
- Keep it tasteful and recognisable. Do not invent text, logos or names that are not in the photo; set name to a
  short nickname for the person or "Shinobi" if unsure.
- If something is unclear, omit that field (never write null).
Return the JSON now.
```

## Using it

1. Save the chatbot's answer as `characters/me.json`.
2. Start the game with `--character characters/me.json`, or type `/character characters/me.json` in the game console.
3. Other players receive a validated copy, so you look like yourself to everyone in the match.

## Implementation notes (deviations and details)

- Wire size is 41 bytes (colours 5-6-5 bits, numbers in 0.01 steps, name 6 bits per character), not ~100. v1.1 grew it
  from 39 bytes, so the network protocol version went from 17 to 18 (old and new builds cannot join the same match). Because of the
  quantisation a colour on screen can differ from your hex by up to ~3%.
- The display name is upper-cased (`[A-Z0-9 _-]`). Colour fields left out keep the stock look for that part (the match
  colour); `weapons.edge` / `weapons.star` fall back to `colors.glow`.
- `colors.glow` is lifted to value >= 0.6 and `colors.suit_base` is capped at value 0.35 (hue kept).
- The greaves and knee bands stay in the match colour unless the character wears `tabi_glow`, so teams still read from the legs.
- `torso.emblem` 1..15 draws a small glyph on the chest in the glow colour (0 keeps the stock core diamond).
- Files over 16 KiB, deeper than 12 levels or with more than 2000 values are refused. The remembered file path is stored
  beside `settings.cfg` as `character.path`.

## Rules the game enforces (so every character stays fair and readable)

- The hitbox, speed and weapons are identical for every character. Looks only.
- `glow` is raised to at least 60% brightness and `suit_base` is capped dark, so no one disappears into the background.
- Players are still tinted by their match colour on the distance marker and outline, so teams and enemies stay clear.
- No uploaded images or text from the file are shown to other players beyond the 16-character name.
- The spec is versioned. Future versions (`shinobi-character/2`) can add fields such as a face texture; old files keep working.
- Expressions, ears, horns, nose and tail are looks only and hide at distance; they never change the hitbox or the readable core.
