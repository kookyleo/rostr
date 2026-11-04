//! Local observations only. No transport, process runner, or credential reader is exposed.
pub mod claude;
pub mod cli;
mod native;
mod repository;
pub use native::{CodexNativeProbe, GrokBuildNativeProbe, native_probes};
mod snapshot;
pub use repository::QuotaRepository;
pub use snapshot::{LocalSnapshotProbe, import_file};

use crate::discover::Env;
use serde::{Deserialize, Serialize};

pub type Timestamp = i64; // Unix milliseconds, UTC.
pub type ProbeId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentId {
    ClaudeCode,
    Codex,
    OpenCode,
    GrokBuild,
    GeminiCli,
    Pi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTarget {
    pub agent: AgentId,
    pub instance: String,
    pub model: String,
    pub route: String,
    /// User-assigned opaque account clue; never a credential or email.
    pub account: String,
    pub raw_provider: String,
    pub raw_model: String,
    pub profile: Option<String>,
    pub billing_mode: Option<String>,
}
impl ExecutionTarget {
    pub fn id(&self) -> String {
        serde_json::to_string(self).expect("serializable identity")
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountBinding {
    pub target: ExecutionTarget,
    pub pool_keys: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BindingReport {
    pub bindings: Vec<AccountBinding>,
    pub ambiguities: Vec<String>,
    pub issues: Vec<String>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessPolicy {
    LocalOnly,
}
#[derive(Debug, Clone)]
pub struct ProbeRequest {
    pub access: AccessPolicy,
    pub as_of: Timestamp,
    pub max_age_ms: i64,
}
pub struct ProbeContext<'a> {
    pub env: &'a Env,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeCapabilities {
    pub local_snapshot: bool,
    pub subscription: bool,
    pub quota: bool,
    pub rate_limit: bool,
    pub schema_versions: Vec<u32>,
}
pub trait QuotaProbe {
    fn id(&self) -> ProbeId;
    fn capabilities(&self) -> ProbeCapabilities;
    fn bindings(&self, env: &Env) -> Result<BindingReport, ProbeError>;
    fn observe(
        &self,
        ctx: &ProbeContext<'_>,
        request: &ProbeRequest,
    ) -> Result<ProbeReport, ProbeError>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeErrorKind {
    UnsupportedVersion,
    PermissionDenied,
    InvalidData,
    AuthenticationRequired,
    NetworkUnavailable,
    EndpointRateLimited,
    Timeout,
    PolicyDenied,
    Io,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeError {
    pub kind: ProbeErrorKind,
    pub probe_id: ProbeId,
    pub retryable: bool,
    pub retry_at: Option<Timestamp>,
    pub diagnostic: String,
}
impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.probe_id, self.diagnostic)
    }
}
impl std::error::Error for ProbeError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Requests,
    InputTokens,
    OutputTokens,
    TotalTokens,
    Credits { namespace: String },
    Money { currency: String },
    Percent,
    ConcurrentRequests,
    Opaque { name: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmountState {
    Known,
    Unknown,
    Unlimited,
    NotApplicable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    Hard,
    Advisory,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Authoritative,
    Derived,
    Estimated,
    UserSupplied,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Fixed,
    Rolling,
    Calendar,
    TokenBucket,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaBucket {
    pub pool_key: String,
    pub account: String,
    pub metric: String,
    pub unit: Unit,
    pub window_key: String,
    pub window_kind: WindowKind,
    pub duration_ms: Option<i64>,
    pub state: AmountState,
    pub used: Option<String>,
    pub remaining: Option<String>,
    pub hard_limit: Option<String>,
    pub enforcement: Enforcement,
    pub observed_at: Timestamp,
    pub valid_until: Option<Timestamp>,
    pub reset_at: Option<Timestamp>,
    pub confidence: Confidence,
    #[serde(default)]
    pub quality_flags: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionSnapshot {
    pub account: String,
    pub plan: Option<String>,
    pub status: Option<String>,
    pub billing_mode: Option<String>,
    pub observed_at: Timestamp,
    pub valid_until: Option<Timestamp>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitObservation {
    pub account: String,
    pub pool_key: Option<String>,
    pub blocked_until: Timestamp,
    pub observed_at: Timestamp,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityEvidence {
    pub target_id: String,
    pub blocked: Option<bool>,
    pub observed_at: Timestamp,
    pub valid_until: Timestamp,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
    NotInstalled,
    NoObservation,
    Unsupported,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSource {
    pub kind: String,
    pub schema_version: u32,
    pub event_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReport {
    pub probe_id: ProbeId,
    pub version: u32,
    pub started_at: Timestamp,
    pub collected_at: Timestamp,
    pub source: ObservationSource,
    pub completeness: Completeness,
    #[serde(default)]
    pub bindings: Vec<AccountBinding>,
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionSnapshot>,
    #[serde(default)]
    pub buckets: Vec<QuotaBucket>,
    #[serde(default)]
    pub rate_limits: Vec<RateLimitObservation>,
    #[serde(default)]
    pub availability: Vec<AvailabilityEvidence>,
    #[serde(default)]
    pub issues: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct QuotaRequirement {
    pub metric: String,
    pub unit: Unit,
    pub min_remaining: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfidencePolicy {
    Any,
    AuthoritativeOnly,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownPolicy {
    Include,
    Exclude,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BillingPolicy {
    Any,
    SubscriptionOnly,
}
#[derive(Debug, Clone)]
pub struct TargetQuery {
    pub agent: Option<AgentId>,
    pub models: Vec<String>,
    pub requirements: Vec<QuotaRequirement>,
    pub min_confidence: ConfidencePolicy,
    pub unknown: UnknownPolicy,
    pub billing: BillingPolicy,
    pub max_age_ms: i64,
    pub as_of: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    Eligible,
    Blocked,
    Unknown,
}
#[derive(Debug, Clone, Serialize)]
pub struct PoolProvenance {
    pub pool_key: String,
    pub probe_id: ProbeId,
    pub source: ObservationSource,
    pub collected_at: Timestamp,
}
#[derive(Debug, Clone, Serialize)]
pub struct TargetQuota {
    pub target: ExecutionTarget,
    pub pools: Vec<QuotaBucket>,
    pub provenance: Vec<PoolProvenance>,
    pub eligibility: Eligibility,
    pub reasons: Vec<String>,
    pub as_of: Timestamp,
}

/// Exact fixed-point parsing; no floating point or lexicographic comparisons.
pub(crate) fn decimal(s: &str) -> anyhow::Result<i128> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    anyhow::ensure!(
        !whole.is_empty()
            && whole.bytes().all(|c| c.is_ascii_digit())
            && frac.bytes().all(|c| c.is_ascii_digit())
            && frac.len() <= 6,
        "expected nonnegative decimal with at most six fractional digits"
    );
    whole
        .parse::<i128>()?
        .checked_mul(1_000_000)
        .and_then(|v| {
            v.checked_add(frac.parse::<i128>().unwrap_or(0) * 10i128.pow(6 - frac.len() as u32))
        })
        .ok_or_else(|| anyhow::anyhow!("decimal out of range"))
}
pub fn now() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("UTC clock")
        .as_millis() as i64
}
