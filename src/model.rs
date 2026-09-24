use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InboxTab {
    #[default]
    MyPrs,
    Repositories,
    Diffs,
}
impl InboxTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::MyPrs => "My PRs",
            Self::Repositories => "Repositories",
            Self::Diffs => "Diffs",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrState {
    #[default]
    Open,
    Merged,
    Closed,
    All,
}
impl PrState {
    pub const ALL: [Self; 4] = [Self::Open, Self::Merged, Self::Closed, Self::All];
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Merged => "Merged",
            Self::Closed => "Closed",
            Self::All => "All",
        }
    }
}

pub fn validate_repository(value: &str) -> anyhow::Result<()> {
    let (owner, repo) = value
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Expected owner/repository"))?;
    PrKey {
        owner: owner.into(),
        repo: repo.into(),
        number: 1,
    }
    .validate()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrKey {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrKey {
    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
    pub fn id(&self) -> String {
        format!("{}#{}", self.repository(), self.number)
    }
    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.repo, self.number
        )
    }
    pub fn from_url(value: &str) -> anyhow::Result<Self> {
        let url = url::Url::parse(value)?;
        anyhow::ensure!(
            url.scheme() == "https" && url.host_str() == Some("github.com"),
            "Use a GitHub PR URL: https://github.com/owner/repo/pull/123"
        );
        let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
        let [owner, repo, "pull", number, ..] = parts.as_slice() else {
            anyhow::bail!("Expected a GitHub pull request URL");
        };
        let key = Self {
            owner: (*owner).into(),
            repo: (*repo).into(),
            number: number.parse()?,
        };
        key.validate()?;
        Ok(key)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        for value in [&self.owner, &self.repo] {
            anyhow::ensure!(
                !value.is_empty()
                    && value != "."
                    && value != ".."
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
                "Invalid GitHub repository name"
            );
        }
        anyhow::ensure!(self.number > 0, "PR number must be positive");
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrSummary {
    pub key: PrKey,
    pub title: String,
    pub author: String,
    pub updated: String,
    pub created: String,
    pub stats: Option<PrStats>,
    #[serde(skip)]
    pub stats_error: bool,
    pub draft: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PrStats {
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrDetail {
    #[serde(default)]
    pub requested_reviewers: Vec<String>,
    #[serde(default)]
    pub requested_teams: Vec<String>,
    pub key: PrKey,
    pub title: String,
    pub body: String,
    pub author: String,
    pub head: String,
    pub base: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
}

#[derive(Clone, Debug, Default)]
pub struct TimelineItem {
    pub date: String,
    pub author: String,
    pub kind: String,
    pub body: String,
    pub url: String,
}

#[derive(Clone, Debug, Default)]
pub struct Check {
    pub name: String,
    pub state: String,
    pub started: String,
    pub completed: String,
    pub url: String,
}

/// Live GitHub state, independent of the pinned review snapshot.
#[derive(Clone, Debug, Default)]
pub struct CheckReport {
    pub checks: Vec<Check>,
    pub mergeable: String,
    pub merge_state: String,
    pub head: String,
    pub base: String,
    pub state: String,
    pub rules_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelChoice {
    pub model: String,
    pub effort: String,
}
impl Default for ModelChoice {
    fn default() -> Self {
        Self {
            model: "gpt-5.6-luna".into(),
            effort: "high".into(),
        }
    }
}
impl ModelChoice {
    pub fn conflict_default() -> Self {
        Self {
            model: "gpt-6-astra".into(),
            effort: "high".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModelPurpose {
    #[default]
    Guide,
    Conflicts,
}
impl ModelPurpose {
    pub fn label(self) -> &'static str {
        match self {
            Self::Guide => "Default guide model",
            Self::Conflicts => "Default conflict resolve model",
        }
    }
    pub fn recommended(self) -> ModelChoice {
        match self {
            Self::Guide => ModelChoice::default(),
            Self::Conflicts => ModelChoice::conflict_default(),
        }
    }
}
impl std::fmt::Display for ModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} · {}", self.model, self.effort)
    }
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub efforts: Vec<String>,
}

/// GitHub and model output are untrusted terminal text. Retain text and newlines,
/// never terminal escape sequences or other control characters.
pub fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}
