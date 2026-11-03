//! The review's life at the provider, kept apart from the pipeline run's
//! state. A report is associated with the commit it reviewed and waits in the
//! `awaiting_human_review` gate; the provider is only ever observed. A merge
//! or closure ends the gate, a different head marks the report stale, and a
//! provider that cannot be asked leaves everything as it was (fail closed).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderState {
    Open,
    Merged,
    Closed,
}

impl ProviderState {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderState::Open => "open",
            ProviderState::Merged => "merged",
            ProviderState::Closed => "closed",
        }
    }

    pub fn parse(name: &str) -> Option<ProviderState> {
        [ProviderState::Open, ProviderState::Merged, ProviderState::Closed].into_iter().find(|s| s.as_str() == name)
    }
}

/// One provider review found for a run's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub review_id: String,
    pub url: String,
    pub state: ProviderState,
}

/// Which review a run tracks, and the others found beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub primary: Option<Candidate>,
    pub duplicates: Vec<Candidate>,
}

/// Pick the one review to track among duplicates. The recorded review wins
/// while it is open or merged; otherwise open beats merged beats closed, and
/// within a state the newest (highest numeric id) wins.
pub fn reconcile(found: &[Candidate], recorded_id: Option<&str>) -> Reconciled {
    let rank = |c: &Candidate| {
        let state = match c.state {
            ProviderState::Open => 2u8,
            ProviderState::Merged => 1,
            ProviderState::Closed => 0,
        };
        (state, c.review_id.parse::<u64>().unwrap_or(0), c.review_id.clone())
    };
    let recorded = recorded_id.and_then(|id| found.iter().position(|c| c.review_id == id && c.state != ProviderState::Closed));
    let chosen = recorded.or_else(|| (0..found.len()).max_by_key(|&i| rank(&found[i])));
    let Some(chosen) = chosen else { return Reconciled { primary: None, duplicates: Vec::new() } };
    let mut duplicates: Vec<Candidate> = found.iter().enumerate().filter(|(i, _)| *i != chosen).map(|(_, c)| c.clone()).collect();
    duplicates.sort_by(|a, b| rank(b).cmp(&rank(a)));
    Reconciled { primary: Some(found[chosen].clone()), duplicates }
}

/// Whether the review still waits for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    AwaitingHumanReview,
    Resolved,
}

impl Gate {
    pub fn as_str(self) -> &'static str {
        match self {
            Gate::AwaitingHumanReview => "awaiting_human_review",
            Gate::Resolved => "resolved",
        }
    }

    pub fn parse(name: &str) -> Option<Gate> {
        [Gate::AwaitingHumanReview, Gate::Resolved].into_iter().find(|g| g.as_str() == name)
    }
}

/// What the provider said about the review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub state: ProviderState,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationError {
    Unavailable(String),
    PermissionDenied(String),
}

/// Text from a provider as one plain line: control characters (escape sequences, line breaks)
/// become spaces, so it cannot move the cursor or pose as another line when printed.
pub fn plain_line(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

impl ObservationError {
    pub fn describe(&self) -> String {
        match self {
            ObservationError::Unavailable(detail) => format!("the provider could not be reached: {}", plain_line(detail)),
            ObservationError::PermissionDenied(detail) => format!("permission to read the review was denied: {}", plain_line(detail)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLifecycle {
    pub provider: String,
    pub review_id: String,
    pub url: String,
    /// The commit the report was written for.
    pub reported_head_sha: String,
    /// The head the provider last showed, when it differs from the report's.
    pub observed_head_sha: Option<String>,
    pub state: ProviderState,
    pub gate: Gate,
    pub stale: bool,
    pub last_error: Option<String>,
}

impl ReviewLifecycle {
    /// A newly pushed review: open, on the reported commit, waiting for a person.
    pub fn associate(provider: &str, review_id: &str, url: &str, reported_head_sha: &str) -> ReviewLifecycle {
        ReviewLifecycle {
            provider: provider.into(),
            review_id: review_id.into(),
            url: url.into(),
            reported_head_sha: reported_head_sha.into(),
            observed_head_sha: None,
            state: ProviderState::Open,
            gate: Gate::AwaitingHumanReview,
            stale: false,
            last_error: None,
        }
    }

    /// The lifecycle after one read-only look at the provider.
    pub fn apply(&self, observation: Result<Observation, ObservationError>) -> ReviewLifecycle {
        let mut next = self.clone();
        let seen = match observation {
            Ok(seen) => seen,
            Err(error) => {
                next.last_error = Some(error.describe());
                return next;
            }
        };
        next.last_error = None;
        // A merge is final: a provider that later reports otherwise changes nothing.
        if self.state == ProviderState::Merged {
            return next;
        }
        next.state = seen.state;
        next.gate = if seen.state == ProviderState::Open { Gate::AwaitingHumanReview } else { Gate::Resolved };
        if seen.head_sha == self.reported_head_sha {
            next.stale = false;
            next.observed_head_sha = None;
        } else {
            next.stale = true;
            next.observed_head_sha = Some(seen.head_sha);
        }
        next
    }
}
