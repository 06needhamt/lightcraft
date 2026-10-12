---
title: A raw photo opens as "Preview only"
description: Why LightCraft shows a raw photo as Preview only, and how to make it decode after an update with Reload from Disk.
order: 10
verified: 0.6.0
---

Select the photo and choose **Photo** > **Reload from Disk**. If your version can decode that raw now, LightCraft reads the sensor data again and the **Preview only** notice goes away.

## Symptoms

- The **Edit** panel shows **Preview only**, with the text "LightCraft can't decode this raw variant yet" and the name of the variant.
- You updated LightCraft to a version that supports your camera, and the photo still looks the same.

## Fix

1. In the grid, select the photos that show **Preview only**.
2. Choose **Photo** > **Reload from Disk**. The right-click menu on a photo has the same entry.
3. Open a photo again. If **Preview only** is gone, the photo is now decoded from the raw data.

If **Preview only** stays, your version still cannot decode that raw variant. Update to the [latest release](https://github.com/storytold/lightcraft/releases/latest) and try again, or report the file as described in [Get help with a problem](get-help.md).

## Why this happens

When LightCraft cannot decode a raw on import, it remembers that and shows the camera's embedded JPEG. It does not check again after an update on its own. **Reload from Disk** is the way to make it check, and importing the photos again works too. Compressed Olympus raws and some Canon CR3 variants are examples of files that use the embedded preview.

Photos shown from the preview keep the camera's picture style and white balance, because they are the camera's own JPEG.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft) or open an issue in the [issue tracker](https://github.com/storytold/lightcraft/issues).
