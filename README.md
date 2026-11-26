# Jade for Sunrise

<p align="center"><img src="docs/images/icon.png" alt="Jade" width="128"></p>

Jade 26.3.5 (Snownee, [CC BY-NC-SA 4.0](LICENSE)) as a native mod on Sunrise's mod API. It uses hooks only, with no
`[native.*]` rows. It shows what you are looking at: the block, fluid or entity, its mod, its harvest tool, and what's
inside. The code is a rewrite of Jade's logic on Sunrise's API, checked against the decompiled jar. Jade's language
files, GUI sprites and themes are included unchanged. This is an unofficial port: see `NOTICE` for credits.

- **Mod id:** `jade`. The version is in `sunrise-mod.toml`.
- **Game build:** pinned to Sunrise Dev 1 (`dev-2026-11-25-bdad86d`), commit `bdad86d` (`sunrise-mod.toml`), Minecraft 26.3. The launcher
  compiles the mod against that build's prebuilt game, so a newer build needs a repinned release.
- **Install:** find Jade in Discover in Sunrise's launcher.
- **Licence:** CC BY-NC-SA 4.0, like Jade itself: free to use and share, not for commercial use, and changes are
  shared under the same licence.

![Jade's overlay on an Iron Furnaces furnace: energy, contents and mod](docs/images/1-overlay.jpg)

## What it does

- **The overlay** for blocks, fluids and entities: title, icon, harvest tool, mod name, breaking progress, containers'
  contents and energy, status effects, and other mods' lines (through the provider API below). Lite mode, extended
  reach, ignore lists, scale, position, opacity and fade, as in Jade.
- **Themes:** Jade's seven, and any a resource pack adds (`assets/<ns>/jade_themes/**.json`).
- **Config screens and profiles:** the four screens, search, and profiles #0 to #3 (`config/jade/profile-<n>.json`).
- **Keys** (Controls, category Jade): Open Config, Show Overlay, Toggle Fluid, Show Recipes, Show Uses, Narrate, Show
  Details, Use Profile #0 to #3.
- **Commands:** `/jadec config | pin | handlers | use_profile <n> | generate_server_overrides` (client),
  `/jade show|hide` (operators), and the game rule `jade:max_position_deviation`.
- **Multiplayer safety:** the server answers only for blocks and entities in the player's level and reach, caps the
  size and rate of requests, keeps command blocks' commands and loot tables to operators, and shows a locked container
  as locked.

## Differences from the original

Each of these waits on a piece of Sunrise's mod API, filed on Sunrise:

- **Block properties.** The fire ignite and burn odds lines are missing (sunrise-org/sunrise#5492). The jump and speed
  factor lines show for blocks whose factor isn't 1.
- **Mod names.** Mods that don't name themselves through Jade's provider API or a language key show a name made from
  their id. A few common ones are named by hand (sunrise-org/sunrise#5494).
- **Entities on a server without Jade.** The client can't read an entity's type, item, pose or synced data yet, so on
  a server without Jade an entity shows no overlay (sunrise-org/sunrise#4741, #5321, #5498). With Jade on the server,
  every line shows.
- **Dropped items' tooltip lines.** The item's own tooltip lines aren't shown under a dropped item
  (sunrise-org/sunrise#5332).
- **Picked items.** A block's icon is its pick-block item and an entity's icon is the game's own pick result. Only a
  block or entity that picks a different item than its plain one on a server without Jade still shows the plain item
  (sunrise-org/sunrise#5495).
- **Ignore lists.** "Edit ignore list in Files…" prints the files' paths in chat instead of opening the folder
  (sunrise-org/sunrise#5336).
- **Narrator.** Narrate stays silent while the Narrator option is "Chat only"; Jade speaks then (sunrise-org/sunrise#5496).

These are not done yet in the port itself:

- **Provider API.** Other mods get Jade's main provider calls, but not provider priorities, the render, ray-trace and
  tooltip callbacks, custom config types or the harvest tool registry.
- **Pin.** `/jadec pin` pins the overlay in place, without Jade's interactive pin screen.
- **Config screens.** They keep each change at once, with Done instead of Save & Quit and Cancel. Other mods' options
  are on/off toggles only.
- **Smaller things:**
  - a campfire's cook time is shown as text, not over the item;
  - plural and gender forms in translations always take "other", and right-to-left layout isn't supported;
  - the REI, Polydex and Tech Reborn energy compatibility and the debug plugin aren't ported. Recipe lookup works
    through JEI.

## For other mods

Jade's provider API is in `src/api.rs`: a mod adds lines for its own blocks and entities.

## Build

`cargo sunrise-mod build` in this folder makes `target/sunrise-mod/jade.smod`.
