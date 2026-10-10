//! `slideshow.playback`: the slideshow's Playback settings (slide and fade durations, fade colour,
//! random order, repeat, preparing previews in advance). A library preference, saved in the
//! library's `prefs.json`; the app's slideshow (View ▸ Slideshow) plays with them.

use serde_json::Value;

use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};

fn playback(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slideshow.merged(p).map_err(|e| bad("slideshow.playback", e))?;
        if next != s.slideshow {
            s.slideshow = next;
            s.save_prefs()?;
        }
    }
    Ok(s.slideshow.to_json())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "slideshow.playback",
        "Slideshow Playback Settings",
        [],
        None,
        "{slideDuration?: seconds (0.5–120), fadeDuration?: seconds (0 = cut; at most 10 and the slide duration), fadeColor?: \"#rrggbb\"|[r,g,b]|null (fade through a colour instead of crossfading), randomOrder?: bool, repeat?: bool, preparePreviews?: bool (render the next slides before they are due)} — omitted keys keep their value, {} reads them; saved with the library → {slideDuration, fadeDuration, fadeColor, randomOrder, repeat, preparePreviews}",
        always,
        playback
    )]
}
