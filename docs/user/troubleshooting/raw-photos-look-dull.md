---
title: Raw photos look dull or washed out
description: What to do when a raw photo looks flat, grey or desaturated in LightCraft compared with the camera JPEG.
order: 20
verified: 0.6.0
---

Update to the latest LightCraft, then import the photo again. Colour handling for raw files changed a lot between 0.4.0, 0.5.0 and 0.6.0, and several flat-looking Sony and Fujifilm cases are fixed. Some Leica DNGs still look dull.

## Symptoms

- A raw photo looks flat, grey or desaturated as soon as you open it.
- The preview or thumbnail looks right, but the opened photo does not.
- Blacks look lifted, and the shadows look grey.

## Fix

1. Check your version in **Help** > **About LightCraft**. If it is older than 0.6.0, install the [latest release](https://github.com/storytold/lightcraft/releases/latest). See [Install LightCraft](../getting-started/install.md).
2. Import the raw photo again, or select it and choose **Photo** > **Reload from Disk**.
3. If it still looks dull, adjust it by hand in the **Edit** panel. See [Edit a photo](../guides/edit-a-photo.md).
4. If you think the starting look is wrong for your camera, report it with the original raw file. See [Get help with a problem](get-help.md).

## Why this happens

LightCraft develops a raw from the sensor data and tries to match the look of the camera's own JPEG. How well it matches depends on the camera model.

- **Sony ARW:** on 0.5.0 and older, some Lossless Compressed M-size files had lifted blacks, and an A7 II file lost most of its colour. Both were fixed after 0.5.0. The A7 II colour fix is in 0.6.0. Some files still fall short on dark scenes and on in-camera Dynamic Range Optimizer shots.
- **Fujifilm X-Pro1 and Sony A7 IV:** reported dull on 0.4.0 and fine on 0.6.0.
- **Leica DNG:** these files carry a colour matrix but no tone curve, so 0.6.0 shows them darker and flatter than the camera JPEG. A fix was waiting for review at the time of writing.
- **Adobe RGB:** if the camera was set to Adobe RGB, LightCraft read the embedded JPEG as sRGB and matched to a too-dull target. The fix came after 0.6.0 and ships in a later release.

## A soft picture after zooming in

If a large raw looks soft right after you zoom in, wait a few seconds. LightCraft first shows the fit-size picture enlarged, then renders the zoomed area from the full-resolution raw and swaps it in. On a 61 MP file this can take a few seconds. If it stays soft at **Zoom 100%** (**View** > **Zoom 100%**), report it.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft) or open an issue in the [issue tracker](https://github.com/storytold/lightcraft/issues).
