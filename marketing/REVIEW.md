# Gallery revision for 0.1.7

The first rejection asked for official hardware resources and a private verification
video. The owner replaced the gallery with the three current device images and
emailed the video. The subsequent rejection specifically asks for accuracy of
images displayed **on devices**. The accepted private demo is unchanged.

The exact remaining objection cannot be established from the wording alone.
These concrete discrepancies were present in the repository's existing gallery:

- `strip.js` hand-picked names, icons, countdowns and bars instead of using captured
  plugin feedback. Its Nunu Flash countdown did not agree with the Nunu key, and it
  included a quest Teleport despite other earlier timers that should sort ahead of it.
- The XL projected a single rectangular 8x4 grid onto a photo with different screen
  sizes and row positions. This covered parts of the key glass and did not follow
  the actual individual LCD areas.
- The XL's `MAIN` artwork was invented instead of captured from a configured action.
- Some copy claimed automatic rune detection without explaining the optional Riot
  API key, and claimed Flash is always a tap even though spell order is configurable.

The revised PNGs use the existing official source photos, captured key SVGs and
`setFeedback` payloads from one run of the real plugin. The touch-strip elements
use the actual plugin layout coordinates and font sizes. XL overlays are mapped
per screen; its base photo is desaturated to avoid colored reflections from the
original stock demo artwork. Unused keys are blank. The dedicated feature image
shows the captured green ready state, haste indicators and Undo action.

These are composites using actual plugin output and official hardware photos;
they are not photographs of a physical device running the plugin. Approval still
depends on Elgato's review. A real hardware photo is the strongest fallback if the
reviewer requests closer evidence of on-device appearance.

AI tools assisted with code, text and gallery composition. The owner confirmed
that the existing product logo was AI-generated. Each gallery image carries a
short disclosure, and the listing draft in `MARKETPLACE.md` explicitly declares
these uses. Official hardware photography and captured screen output are distinct
from the AI-generated logo. The private video has not been regenerated.

Upload these three files as the gallery items:

- `gallery-1-hero.png`
- `gallery-2-keys.png`
- `gallery-3-devices.png`

All are PNGs at 1920x960 with English text. The private emailed video is not a
gallery item; its previous acceptance does not need to be revisited for this change.

Current official requirements: [Elgato Product Guidelines: Gallery](https://docs.elgato.com/guidelines/products/#gallery).
Official photo source: [Elgato Media Room](https://www.elgato.com/s/media-room).

Rebuild with `powershell -ExecutionPolicy Bypass -File tools/build-gallery.ps1`.
Capture uses a simulated game but runs the real plugin executable and renderer.
