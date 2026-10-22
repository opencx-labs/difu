//! Preferences, registry metadata, and safe draft transactions shared by Agents and Reviews.
mod draft;
mod registry;
mod types;
pub use draft::{SaveError, SettingsDraft, export_preferences};
pub use registry::{
    Category, Effect, Preview, SettingId, SettingKind, SettingSpec, SettingValue, get, registry,
    search, set, spec, validate,
};
pub use types::*;

#[cfg(test)]
mod tests;
