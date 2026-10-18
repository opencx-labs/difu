//! Session-local sidebar filters. Matching uses in-memory lists, without a GitHub search.
use crate::{editor::Editor, model::PrSummary};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Repositories,
    PullRequests,
    Files,
}
#[derive(Default)]
pub struct State {
    pub repositories: Editor,
    pub prs: Editor,
    pub files: Editor,
    pub focused: Option<Kind>,
}
impl State {
    pub fn editor(&self, kind: Kind) -> &Editor {
        match kind {
            Kind::Repositories => &self.repositories,
            Kind::PullRequests => &self.prs,
            Kind::Files => &self.files,
        }
    }
    pub fn editor_mut(&mut self, kind: Kind) -> &mut Editor {
        match kind {
            Kind::Repositories => &mut self.repositories,
            Kind::PullRequests => &mut self.prs,
            Kind::Files => &mut self.files,
        }
    }
}
pub fn matches(query: &str, value: &str) -> bool {
    value.to_lowercase().contains(&query.to_lowercase())
}
pub fn prs(prs: &[PrSummary], query: &str) -> Vec<usize> {
    prs.iter()
        .enumerate()
        .filter_map(|(index, pr)| {
            (matches(query, &pr.title) || matches(query, &pr.author)).then_some(index)
        })
        .collect()
}
