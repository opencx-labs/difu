//! Clone large transcript payloads by reference; copy only the payload being edited.
use serde::{Deserialize, Serialize};
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Shared<T>(Arc<T>);
impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(value))
    }
}
impl From<&str> for Shared<String> {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}
impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}
impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}
impl PartialEq<String> for Shared<String> {
    fn eq(&self, other: &String) -> bool {
        **self == *other
    }
}
impl PartialEq<&str> for Shared<String> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
impl std::fmt::Display for Shared<String> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_share_unchanged_payloads_and_edits_are_isolated() -> anyhow::Result<()> {
        let original: Shared<String> = "history".into();
        let mut next = original.clone();
        assert!(Arc::ptr_eq(&original.0, &next.0));
        next.push_str(" update");
        assert_eq!(original, "history");
        assert_eq!(next, "history update");
        assert_eq!(serde_json::to_string(&original)?, "\"history\"");
        assert_eq!(
            serde_json::from_str::<Shared<String>>("\"history\"")?,
            original
        );
        Ok(())
    }
}
