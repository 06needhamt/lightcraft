//! The settings a print is made from, and (with the saved-template commands) what a print
//! template stores.
use super::PrintJob;
use super::compose::{ImageSettings, PageStyle};
use super::layout::{Guides, Layout};
use serde::{Deserialize, Serialize};

/// Layout, image settings, page, guides and job: the whole Print module state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PrintSettings {
    pub layout: Layout,
    pub image: ImageSettings,
    pub page: PageStyle,
    pub guides: Guides,
    pub job: PrintJob,
}

impl PrintSettings {
    /// Checks every part; run on anything that did not come from the setters.
    pub fn validate(&mut self) -> Result<(), String> {
        self.layout.validate()?;
        self.image.validate()?;
        self.guides.validate()?;
        self.job.validate()
    }
}
