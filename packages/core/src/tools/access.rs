use std::sync::{Arc, Mutex};

/// What a tool may do to the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolAccess {
    ReadOnly,
    Execute,
}

impl ToolAccess {
    /// Whether a run in `self` mode may call a tool with `required` access.
    pub fn allows(self, required: ToolAccess) -> bool {
        self == Self::Execute || required == Self::ReadOnly
    }
}

/// What the user asked for this turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Change the code now.
    Implement,
    /// Assess a possible change; stop at a plan.
    Evaluate,
    /// Revise a plan that is already under review.
    Revise,
    /// Answer a question without changing anything.
    Question,
}

/// Whether the change reaches the architecture model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchitectureImpact {
    /// Confined to existing components, with no element or dependency change.
    None,
    /// Adds, removes, or re-scopes elements, or changes a dependency.
    Possible,
    /// The user explicitly asks for an architectural change.
    Confirmed,
}

/// The model's classification of one turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assessment {
    pub intent: Intent,
    pub impact: ArchitectureImpact,
}

impl Assessment {
    /// Whether this classification opens the execution tools: the user asked
    /// to implement now and the change stays inside existing components.
    pub fn opens_execution(self) -> bool {
        self.intent == Intent::Implement && self.impact == ArchitectureImpact::None
    }

    /// Whether this turn ends in a plan that needs user review instead of edits.
    pub fn needs_plan(self) -> bool {
        match self.intent {
            Intent::Implement => self.impact != ArchitectureImpact::None,
            Intent::Evaluate | Intent::Revise => true,
            Intent::Question => false,
        }
    }
}

/// Per-run capability gate.
///
/// A normal turn executes like the agent always has. A turn started with
/// `@arch` is planning-only: the model classifies it with `assess_change`, but
/// execution stays closed until the user confirms the plan card. A run started
/// by a background notice is locked and can never open execution either.
pub struct Access {
    state: Mutex<AccessState>,
}

pub(crate) const PLAN_BASELINE_CHANGED: &str = "Architecture sources changed during planning. Start a new planning turn and query the current model before validating the draft.";

struct AccessState {
    mode: ToolAccess,
    locked: bool,
    /// Architecture planning runs never open execution, even after the model
    /// classifies the turn as a direct implementation.
    planning_only: bool,
    /// Whether the run may use the plan-only tools.
    plan_tools: bool,
    assessment: Option<Assessment>,
    plan_baseline: Option<String>,
    plan_baseline_stale: bool,
    execution_revoked: bool,
}

impl Access {
    fn new(mode: ToolAccess, locked: bool, planning_only: bool, plan_tools: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AccessState {
                mode,
                locked,
                planning_only,
                plan_tools,
                assessment: None,
                plan_baseline: None,
                plan_baseline_stale: false,
                execution_revoked: false,
            }),
        })
    }

    /// A turn that can never write, such as a background-notice wake.
    pub fn locked_read_only() -> Arc<Self> {
        Self::new(ToolAccess::ReadOnly, true, false, false)
    }

    /// Full access, used by callers outside the chat flow such as compaction.
    pub fn execute() -> Arc<Self> {
        Self::new(ToolAccess::Execute, false, false, false)
    }

    /// An `@arch` turn: read-only until the user confirms the plan card.
    pub fn planning() -> Arc<Self> {
        Self::new(ToolAccess::ReadOnly, false, true, true)
    }

    /// Begin implementing a reviewed plan. A new planning assessment can
    /// revoke execution, which then requires another reviewed plan.
    pub fn approved() -> Arc<Self> {
        Self::new(ToolAccess::Execute, true, false, true)
    }

    pub fn mode(&self) -> ToolAccess {
        self.lock().mode
    }

    pub fn is_locked(&self) -> bool {
        self.lock().locked
    }

    pub fn planning_only(&self) -> bool {
        self.lock().planning_only
    }

    pub fn plan_tools(&self) -> bool {
        self.lock().plan_tools
    }

    pub fn assessment(&self) -> Option<Assessment> {
        self.lock().assessment
    }

    pub(super) fn plan_baseline_changed(&self, digest: Option<&str>) -> bool {
        let mut state = self.lock();
        if let Some(original) = state.plan_baseline.as_deref() {
            state.plan_baseline_stale |= Some(original) != digest;
        } else {
            state.plan_baseline = digest.map(str::to_owned);
        }
        state.plan_baseline_stale
    }

    pub(crate) fn plan_baseline_stale(&self) -> bool {
        self.lock().plan_baseline_stale
    }

    /// Record the model's classification and return the mode it activates.
    pub fn assess(&self, assessment: Assessment) -> ToolAccess {
        let mut state = self.lock();
        if state.planning_only {
            let mut assessment = assessment;
            // `@arch` always ends at the review card. Re-label a direct
            // implementation as an assessment so the turn still records a plan.
            if assessment.opens_execution() {
                assessment.intent = Intent::Evaluate;
            }
            state.assessment = Some(assessment);
            state.mode = ToolAccess::ReadOnly;
            return state.mode;
        }
        if state.execution_revoked {
            return state.mode;
        }
        state.assessment = Some(assessment);
        if state.mode == ToolAccess::Execute && assessment.needs_plan() {
            state.mode = ToolAccess::ReadOnly;
            state.locked = true;
            state.execution_revoked = true;
            state.plan_baseline = None;
            state.plan_baseline_stale = false;
        } else if !state.locked {
            state.mode = if assessment.opens_execution() {
                ToolAccess::Execute
            } else {
                ToolAccess::ReadOnly
            };
        }
        state.mode
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AccessState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
