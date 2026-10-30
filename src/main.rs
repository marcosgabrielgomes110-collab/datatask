use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{
    process::Command,
    sync::{Mutex, Semaphore},
    time,
};

const LOG_RESPONSE_MAX_BYTES: u64 = 65_536;
/// Maximum bytes returned per incremental log poll.
const LOG_CHUNK_MAX_BYTES: u64 = 262_144;
const MAX_ACTION_BYTES: usize = 8 * 1024;
const MAX_DESCRIPTION_BYTES: usize = 2 * 1024;
const MAX_DATETIME_BYTES: usize = 128;
const SHUTDOWN_GRACE_SECONDS: u64 = 10;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    bind_address: String,
    port: u16,
    #[serde(default)]
    api_token: Option<String>,
    #[serde(default)]
    api_token_file: Option<PathBuf>,
    jobs_dir: PathBuf,
    logs_dir: PathBuf,
    poll_interval_seconds: u64,
    command_timeout_seconds: u64,
    #[serde(default = "default_max_concurrent_jobs")]
    max_concurrent_jobs: usize,
    /// Rotate `NAME.log` to `NAME.log.1` once it reaches this size. `0` disables rotation.
    #[serde(default = "default_log_max_bytes")]
    log_max_bytes: u64,
    /// Maximum API requests per IP per minute. `0` disables the limit.
    #[serde(default = "default_rate_limit_per_minute")]
    rate_limit_per_minute: u32,
    /// Failed Bearer attempts allowed per IP before a temporary lockout. `0` disables it.
    #[serde(default = "default_auth_failure_limit")]
    auth_failure_limit: u32,
    #[serde(default = "default_auth_failure_window_seconds")]
    auth_failure_window_seconds: u64,
    #[serde(default = "default_auth_lockout_seconds")]
    auth_lockout_seconds: u64,
    /// Trust `X-Forwarded-For` from a reverse proxy. Keep `false` when exposed directly.
    #[serde(default)]
    trust_proxy_headers: bool,
    /// JSONL audit trail. Empty string disables it; omitted defaults to `<logs_dir>/audit.jsonl`.
    #[serde(default)]
    audit_log_file: Option<PathBuf>,
}

fn default_max_concurrent_jobs() -> usize {
    4
}

fn default_log_max_bytes() -> u64 {
    10 * 1024 * 1024
}

fn default_rate_limit_per_minute() -> u32 {
    600
}

fn default_auth_failure_limit() -> u32 {
    10
}

fn default_auth_failure_window_seconds() -> u64 {
    60
}

fn default_auth_lockout_seconds() -> u64 {
    300
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    repository_lock: Arc<Mutex<()>>,
    running_jobs: Arc<StdMutex<HashSet<String>>>,
    /// PIDs that are also process-group leaders, so shutdown can terminate
    /// commands that outlive the grace period.
    process_groups: Arc<StdMutex<HashSet<u32>>>,
    job_slots: Arc<Semaphore>,
    token_digest: [u8; 32],
    guard: Arc<AccessGuard>,
    audit: Option<Arc<AuditLog>>,
    last_tick: Arc<StdMutex<Option<Instant>>>,
}

impl AppState {
    fn audit(&self, event: Value) {
        if let Some(audit) = &self.audit {
            audit.write(event);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RateLimits {
    per_minute: u32,
    failure_limit: u32,
    failure_window: Duration,
    lockout: Duration,
}

#[derive(Debug)]
struct ClientState {
    window_start: Instant,
    requests: u32,
    failure_window_start: Instant,
    failures: u32,
    locked_until: Option<Instant>,
}

impl ClientState {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            requests: 0,
            failure_window_start: now,
            failures: 0,
            locked_until: None,
        }
    }
}

#[derive(Debug)]
enum Rejection {
    RateLimited(Duration),
    AuthLocked(Duration),
}

impl Rejection {
    fn retry_after(&self) -> Duration {
        match self {
            Self::RateLimited(duration) | Self::AuthLocked(duration) => *duration,
        }
    }

    fn reason(&self) -> &'static str {
        match self {
            Self::RateLimited(_) => "rate_limit",
            Self::AuthLocked(_) => "auth_lockout",
        }
    }
}

/// In-memory per-IP request limiter and failed-authentication lockout.
struct AccessGuard {
    limits: RateLimits,
    clients: StdMutex<HashMap<IpAddr, ClientState>>,
}

impl AccessGuard {
    fn new(limits: RateLimits) -> Self {
        Self {
            limits,
            clients: StdMutex::new(HashMap::new()),
        }
    }

    fn check(&self, ip: IpAddr, now: Instant) -> Result<(), Rejection> {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if clients.len() > 4096 {
            let stale = Duration::from_secs(120);
            clients.retain(|_, state| {
                now.saturating_duration_since(state.window_start) < stale
                    || state.locked_until.is_some_and(|until| until > now)
            });
        }
        let state = clients.entry(ip).or_insert_with(|| ClientState::new(now));

        if let Some(until) = state.locked_until {
            if until > now {
                return Err(Rejection::AuthLocked(until.saturating_duration_since(now)));
            }
            state.locked_until = None;
        }

        if self.limits.per_minute > 0 {
            let window = Duration::from_secs(60);
            if now.saturating_duration_since(state.window_start) >= window {
                state.window_start = now;
                state.requests = 0;
            }
            if state.requests >= self.limits.per_minute {
                let retry =
                    window.saturating_sub(now.saturating_duration_since(state.window_start));
                return Err(Rejection::RateLimited(retry.max(Duration::from_secs(1))));
            }
            state.requests = state.requests.saturating_add(1);
        }
        Ok(())
    }

    /// Records a failed Bearer attempt and returns the lockout duration when
    /// the limit is reached.
    fn record_failure(&self, ip: IpAddr, now: Instant) -> Option<Duration> {
        if self.limits.failure_limit == 0 {
            return None;
        }
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = clients.entry(ip).or_insert_with(|| ClientState::new(now));
        if now.saturating_duration_since(state.failure_window_start) >= self.limits.failure_window {
            state.failure_window_start = now;
            state.failures = 0;
        }
        state.failures = state.failures.saturating_add(1);
        if state.failures >= self.limits.failure_limit {
            state.failures = 0;
            state.failure_window_start = now;
            state.locked_until = Some(now + self.limits.lockout);
            return Some(self.limits.lockout);
        }
        None
    }

    fn record_success(&self, ip: IpAddr) {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(state) = clients.get_mut(&ip) {
            state.failures = 0;
            state.locked_until = None;
        }
    }
}

const AUDIT_MAX_BYTES: u64 = 5 * 1024 * 1024;

struct AuditLog {
    path: PathBuf,
    lock: StdMutex<()>,
}

impl AuditLog {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: StdMutex::new(()),
        }
    }

    fn write(&self, event: Value) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(parent) = self.path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            eprintln!(
                "Cannot create audit directory {}: {error}",
                parent.display()
            );
            return;
        }
        if let Ok(metadata) = fs::metadata(&self.path)
            && metadata.len() >= AUDIT_MAX_BYTES
        {
            let mut rotated = self.path.as_os_str().to_owned();
            rotated.push(".1");
            let rotated = PathBuf::from(rotated);
            let _ = fs::remove_file(&rotated);
            if let Err(error) = fs::rename(&self.path, &rotated) {
                eprintln!("Cannot rotate audit log: {error}");
            }
        }
        match OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
        {
            Ok(mut file) => {
                let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
                if let Err(error) = writeln!(file, "{event}") {
                    eprintln!("Cannot write audit log: {error}");
                    return;
                }
                let _ = file.sync_data();
            }
            Err(error) => eprintln!("Cannot open audit log {}: {error}", self.path.display()),
        }
    }
}

fn client_ip(state: &AppState, request: &Request<Body>) -> IpAddr {
    if state.config.trust_proxy_headers
        && let Some(value) = request
            .headers()
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
        && let Some(ip) = value
            .split(',')
            .next()
            .and_then(|part| part.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

fn job_from_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/jobs/")?;
    let name = rest.split('/').next()?;
    (!name.is_empty()).then_some(name)
}

fn retry_seconds(duration: Duration) -> u64 {
    duration.as_secs_f64().ceil().max(1.0) as u64
}

fn too_many_requests(retry: Duration) -> Response {
    let seconds = retry_seconds(retry);
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"error": "Too many requests. Try again later."})),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

async fn api_guard(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if !path.starts_with("/api/") {
        return next.run(request).await;
    }

    let ip = client_ip(&state, &request);
    if let Err(rejection) = state.guard.check(ip, Instant::now()) {
        let retry = rejection.retry_after();
        state.audit(json!({
            "ts": Local::now().to_rfc3339(),
            "event": "request.rejected",
            "reason": rejection.reason(),
            "method": request.method().as_str(),
            "path": path,
            "ip": ip.to_string(),
            "retry_after_seconds": retry_seconds(retry),
        }));
        return too_many_requests(retry);
    }

    let method = request.method().clone();
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::TOO_MANY_REQUESTS
        || matches!(
            method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        )
    {
        state.audit(json!({
            "ts": Local::now().to_rfc3339(),
            "event": "api.request",
            "method": method.as_str(),
            "path": path,
            "job": job_from_path(&path),
            "status": status.as_u16(),
            "ip": ip.to_string(),
            "duration_ms": started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        }));
    }
    response
}

fn scheduler_readiness(
    last_tick: Option<Instant>,
    interval: Duration,
    now: Instant,
) -> (bool, Option<f64>) {
    match last_tick {
        Some(tick) => {
            let lag = now.saturating_duration_since(tick);
            let max_lag = interval
                .saturating_mul(3)
                .saturating_add(Duration::from_secs(5));
            (lag <= max_lag, Some(lag.as_secs_f64()))
        }
        None => (false, None),
    }
}

/// Removes a job from the running set even if its task panics or is aborted.
struct RunningJobGuard {
    running: Arc<StdMutex<HashSet<String>>>,
    name: String,
}

impl Drop for RunningJobGuard {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.name);
    }
}

fn lock_running(state: &AppState) -> std::sync::MutexGuard<'_, HashSet<String>> {
    state
        .running_jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    name: String,
    description: String,
    datetime: String,
    action: String,
    log: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_duration_ms: Option<u64>,
}

/// Outcome of a single command execution, used to record run history.
struct RunOutcome {
    exit_code: Option<i32>,
    timed_out: bool,
    duration: Duration,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum RunOrigin {
    Manual,
    Scheduled,
}

impl RunOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduler",
        }
    }
}

impl RunOutcome {
    fn success(&self) -> bool {
        !self.timed_out && self.error.is_none() && self.exit_code == Some(0)
    }

    fn label(&self) -> &'static str {
        if self.timed_out {
            "timeout"
        } else if self.success() {
            "success"
        } else {
            "error"
        }
    }
}

/// Optional per-job settings submitted from the panel: `working_dir` may be
/// `None`/omitted or an empty string (clear), and `timeout_seconds` uses `0`
/// as "use the server default / clear".
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateJob {
    name: String,
    description: String,
    datetime: String,
    action: String,
    status: Option<String>,
    working_dir: Option<String>,
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateJob {
    description: Option<String>,
    datetime: Option<String>,
    action: Option<String>,
    status: Option<String>,
    working_dir: Option<String>,
    timeout_seconds: Option<u64>,
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn api_error(status: StatusCode, message: impl Into<String>) -> ApiError {
    ApiError(status, message.into())
}

/// Reads the API token from `api_token` or from a separate private file.
fn resolve_api_token(config: &mut Config, base: &FsPath) -> Result<String, String> {
    if let Some(file) = config.api_token_file.as_mut()
        && file.is_relative()
    {
        *file = base.join(&*file);
    }

    let token = match (config.api_token.take(), config.api_token_file.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(
                "Set only one of api_token or api_token_file in the config file.".to_owned(),
            );
        }
        (Some(token), None) => token.trim().to_owned(),
        (None, Some(path)) => {
            let metadata = fs::metadata(path).map_err(|error| {
                format!("Cannot read API token file {}: {error}", path.display())
            })?;
            if !has_private_permissions(&metadata) {
                return Err(format!(
                    "API token file {} must not be accessible by group or others (chmod 600).",
                    path.display()
                ));
            }
            fs::read_to_string(path)
                .map_err(|error| format!("Cannot read API token file {}: {error}", path.display()))?
                .trim()
                .to_owned()
        }
        (None, None) => {
            return Err(
                "Set api_token or api_token_file in the config file to a secret of at least 32 characters."
                    .to_owned(),
            );
        }
    };

    validate_token(&token)?;
    Ok(token)
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.len() < 32 {
        return Err("api_token must have at least 32 characters.".to_owned());
    }
    if token.len() > 4096 {
        return Err("api_token must be at most 4096 characters.".to_owned());
    }
    if token.chars().any(char::is_whitespace) {
        return Err("api_token must not contain whitespace.".to_owned());
    }
    let lower = token.to_ascii_lowercase();
    // Reject the placeholders shipped in examples/docs so a copied config cannot
    // silently start with a publicly known credential.
    const WEAK_MARKERS: [&str; 7] = [
        "substitua",
        "troque",
        "changeme",
        "replace-",
        "your-token",
        "cole-o-token",
        "example-token",
    ];
    if WEAK_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return Err(
            "api_token looks like a placeholder; generate one with `openssl rand -hex 32`."
                .to_owned(),
        );
    }
    if token
        .as_bytes()
        .first()
        .is_some_and(|first| token.bytes().all(|byte| byte == *first))
    {
        return Err("api_token must not be a single repeated character.".to_owned());
    }
    Ok(())
}

fn has_private_permissions(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o077 == 0
}

fn load_config(path: &FsPath) -> Result<(Config, String), String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("Cannot read config {}: {error}", path.display()))?;
    let mut config: Config = toml::from_str(&text).map_err(|error| {
        // Never echo the offending line: it may contain the API token. Report
        // only the position and the parser message.
        let position = error
            .span()
            .map(|span| {
                let prefix = &text[..span.start.min(text.len())];
                let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
                let column = prefix
                    .rsplit('\n')
                    .next()
                    .map(|line| line.chars().count())
                    .unwrap_or(0)
                    + 1;
                format!(" at line {line}, column {column}")
            })
            .unwrap_or_default();
        format!(
            "Invalid config {}{}: {}",
            path.display(),
            position,
            error.message()
        )
    })?;

    let base = path.parent().unwrap_or_else(|| FsPath::new("."));
    if config.jobs_dir.is_relative() {
        config.jobs_dir = base.join(&config.jobs_dir);
    }
    if config.logs_dir.is_relative() {
        config.logs_dir = base.join(&config.logs_dir);
    }
    config.audit_log_file = match config.audit_log_file.take() {
        None => Some(config.logs_dir.join("audit.jsonl")),
        Some(path) if path.as_os_str().is_empty() => None,
        Some(path) if path.is_relative() => Some(base.join(path)),
        Some(path) => Some(path),
    };
    let token = resolve_api_token(&mut config, base)?;

    if config.poll_interval_seconds == 0 {
        return Err("poll_interval_seconds must be greater than zero.".to_owned());
    }
    if config.command_timeout_seconds == 0 {
        return Err("command_timeout_seconds must be greater than zero.".to_owned());
    }
    if config.max_concurrent_jobs == 0 {
        return Err("max_concurrent_jobs must be greater than zero.".to_owned());
    }
    if config.auth_failure_limit > 0 && config.auth_failure_window_seconds == 0 {
        return Err("auth_failure_window_seconds must be greater than zero.".to_owned());
    }
    if config.auth_failure_limit > 0 && config.auth_lockout_seconds == 0 {
        return Err("auth_lockout_seconds must be greater than zero.".to_owned());
    }
    if let Ok(metadata) = fs::metadata(path)
        && !has_private_permissions(&metadata)
    {
        eprintln!(
            "warning: {} is readable by group/others; run `chmod 600 {}`.",
            path.display(),
            path.display()
        );
    }
    Ok((config, token))
}

fn valid_job_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn job_path(config: &Config, name: &str) -> ApiResult<PathBuf> {
    if !valid_job_name(name) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Job name must contain only letters, numbers, '-' or '_'.",
        ));
    }
    Ok(config.jobs_dir.join(format!("{name}.json")))
}

fn normalize_status(status: &str) -> ApiResult<String> {
    match status.trim().to_lowercase().as_str() {
        "ativo" | "active" | "pending" | "running" => Ok("ativo".to_owned()),
        "desativo" | "inactive" | "completed" | "error" => Ok("desativo".to_owned()),
        _ => Err(api_error(
            StatusCode::BAD_REQUEST,
            "status must be 'ativo' or 'desativo'.",
        )),
    }
}

fn clear_last_run_if_not_current_minute(job: &mut Job) {
    let current_minute = Local::now().format("%Y-%m-%d %H:%M").to_string();
    if job.last_run.as_deref() != Some(&current_minute) {
        job.last_run = None;
    }
}

const MAX_JOB_TIMEOUT_SECONDS: u64 = 86_400;

/// Validates an explicit per-job timeout. `0` means "not set".
fn normalize_timeout(seconds: u64) -> ApiResult<Option<u64>> {
    match seconds {
        0 => Ok(None),
        seconds if seconds <= MAX_JOB_TIMEOUT_SECONDS => Ok(Some(seconds)),
        _ => Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("timeout_seconds must be between 1 and {MAX_JOB_TIMEOUT_SECONDS}."),
        )),
    }
}

/// Validates the working directory for a job. An empty value clears it.
fn normalize_working_dir(value: Option<&str>) -> ApiResult<Option<String>> {
    let Some(trimmed) = value.map(str::trim).filter(|dir| !dir.is_empty()) else {
        return Ok(None);
    };
    if !FsPath::new(trimmed).is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("Directory '{trimmed}' does not exist on the server."),
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

fn validate_text_length(field: &str, value: &str, maximum: usize) -> ApiResult<()> {
    if value.len() > maximum {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("{field} must be at most {maximum} bytes."),
        ));
    }
    Ok(())
}

fn read_job(config: &Config, name: &str) -> ApiResult<Job> {
    let path = job_path(config, name)?;
    let contents = fs::read_to_string(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => api_error(StatusCode::NOT_FOUND, "Job not found."),
        _ => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot read job: {error}"),
        ),
    })?;
    let mut job: Job = serde_json::from_str(&contents).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Invalid job JSON: {error}"),
        )
    })?;
    job.status = normalize_status(&job.status)?;
    job.log = format!("logs/{}.log", job.name);
    Ok(job)
}

fn write_job(config: &Config, job: &Job) -> ApiResult<()> {
    let path = job_path(config, &job.name)?;
    fs::create_dir_all(&config.jobs_dir).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot create jobs directory: {error}"),
        )
    })?;
    let temporary_path = path.with_extension("json.tmp");
    let contents = serde_json::to_vec_pretty(job).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot serialize job: {error}"),
        )
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary_path)
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot write job: {error}"),
            )
        })?;
    fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot secure job file: {error}"),
        )
    })?;
    file.write_all(&contents).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot write job: {error}"),
        )
    })?;
    file.sync_all().map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot flush job: {error}"),
        )
    })?;
    drop(file);
    fs::rename(&temporary_path, path).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot save job: {error}"),
        )
    })?;
    // Make the rename durable too, so a crash cannot leave a missing/old job file.
    if let Ok(directory) = File::open(&config.jobs_dir) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn list_jobs(config: &Config) -> ApiResult<Vec<Job>> {
    let entries = match fs::read_dir(&config.jobs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot list jobs: {error}"),
            ));
        }
    };

    let mut jobs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot read jobs directory entry: {error}"),
            )
        })?;
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            let name = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    api_error(StatusCode::INTERNAL_SERVER_ERROR, "Invalid job filename.")
                })?
                .to_owned();
            // A single corrupt file (manual edit, partial restore) must not take
            // down the whole listing or the panel.
            match read_job(config, &name) {
                Ok(job) => jobs.push(job),
                Err(error) => eprintln!("Skipping invalid job '{name}': {}", error.1),
            }
        }
    }
    jobs.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(jobs)
}

fn parse_datetime(value: &str) -> ApiResult<DateTime<Local>> {
    let formats = ["%d %m %Y %H %M", "%Y-%m-%d %H:%M"];
    for format in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, format) {
            return Local.from_local_datetime(&naive).single().ok_or_else(|| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    "Scheduled local time is ambiguous or invalid for this timezone.",
                )
            });
        }
    }
    Err(api_error(
        StatusCode::BAD_REQUEST,
        "Use cron (minute hour day month weekday) or date/time 'DD MM YYYY HH MM'.",
    ))
}

fn cron_values(field: &str, minimum: u32, maximum: u32) -> Result<(HashSet<u32>, bool), String> {
    if field.is_empty() {
        return Err("Cron field cannot be empty.".to_owned());
    }
    let wildcard = field == "*";
    let mut values = HashSet::new();
    for part in field.split(',') {
        if part.is_empty() {
            return Err(format!("Invalid cron field '{field}'."));
        }
        let (base, step) = match part.split_once('/') {
            Some((base, step)) => {
                let step = step
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| format!("Invalid cron step '{part}'."))?;
                (base, step)
            }
            None => (part, 1),
        };
        let (start, end) = if base == "*" {
            (minimum, maximum)
        } else if let Some((start, end)) = base.split_once('-') {
            let start = start
                .parse::<u32>()
                .map_err(|_| format!("Invalid cron range '{part}'."))?;
            let end = end
                .parse::<u32>()
                .map_err(|_| format!("Invalid cron range '{part}'."))?;
            if start > end {
                return Err(format!("Invalid cron range '{part}'."));
            }
            (start, end)
        } else {
            let value = base
                .parse::<u32>()
                .map_err(|_| format!("Invalid cron value '{part}'."))?;
            (value, if part.contains('/') { maximum } else { value })
        };

        if start < minimum || end > maximum {
            return Err(format!("Cron value out of range '{part}'."));
        }
        let mut current = start;
        while current <= end {
            values.insert(if maximum == 7 && current == 7 {
                0
            } else {
                current
            });
            match current.checked_add(step) {
                Some(next) => current = next,
                None => break,
            }
        }
    }
    Ok((values, wildcard))
}

fn cron_matches(expression: &str, now: &DateTime<Local>) -> Result<bool, String> {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err("Cron expression must have five fields.".to_owned());
    }
    let (minutes, _) = cron_values(fields[0], 0, 59)?;
    let (hours, _) = cron_values(fields[1], 0, 23)?;
    let (days, day_wildcard) = cron_values(fields[2], 1, 31)?;
    let (months, _) = cron_values(fields[3], 1, 12)?;
    let (weekdays, weekday_wildcard) = cron_values(fields[4], 0, 7)?;
    let weekday = now.weekday().num_days_from_sunday();
    let day_matches = match (day_wildcard, weekday_wildcard) {
        (true, true) => true,
        (true, false) => weekdays.contains(&weekday),
        (false, true) => days.contains(&now.day()),
        (false, false) => days.contains(&now.day()) || weekdays.contains(&weekday),
    };
    Ok(minutes.contains(&now.minute())
        && hours.contains(&now.hour())
        && months.contains(&now.month())
        && day_matches)
}

fn schedule_matches(value: &str, now: &DateTime<Local>) -> ApiResult<bool> {
    if let Ok(scheduled) = parse_datetime(value) {
        return Ok(now >= &scheduled);
    }
    cron_matches(value, now).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))
}

fn validate_schedule(value: &str) -> ApiResult<()> {
    if parse_datetime(value).is_ok() {
        return Ok(());
    }
    let fields: Vec<&str> = value.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Use cron (minute hour day month weekday) or date/time 'DD MM YYYY HH MM'.",
        ));
    }
    for (field, (minimum, maximum)) in
        fields
            .iter()
            .zip([(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)])
    {
        cron_values(field, minimum, maximum)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    }
    Ok(())
}

async fn authenticate(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&state, &request);
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let authorized = supplied.is_some_and(|token| {
        // Comparing digests avoids leaking token length and keeps the check
        // constant-time even for differently sized inputs.
        let digest = Sha256::digest(token.as_bytes());
        bool::from(digest.as_slice().ct_eq(&state.token_digest))
    });
    if !authorized {
        let now = Instant::now();
        state.audit(json!({
            "ts": Local::now().to_rfc3339(),
            "event": "auth.failure",
            "ip": ip.to_string(),
            "path": request.uri().path(),
        }));
        if let Some(lockout) = state.guard.record_failure(ip, now) {
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "auth.lockout",
                "ip": ip.to_string(),
                "seconds": lockout.as_secs(),
            }));
        }
        let mut response = (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    state.guard.record_success(ip);
    next.run(request).await
}

const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; \
connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; \
form-action 'none'";

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &'static str) {
    if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
        headers.insert(name, HeaderValue::from_static(value));
    }
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    insert_header(headers, "x-content-type-options", "nosniff");
    insert_header(headers, "x-frame-options", "DENY");
    insert_header(headers, "referrer-policy", "no-referrer");
    insert_header(
        headers,
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=()",
    );
    insert_header(headers, "cross-origin-opener-policy", "same-origin");
    insert_header(headers, "cross-origin-resource-policy", "same-origin");
    insert_header(headers, "content-security-policy", CONTENT_SECURITY_POLICY);
    if is_api {
        insert_header(headers, "cache-control", "no-store");
    }
    response
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    let now = Instant::now();
    let last_tick = *state
        .last_tick
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let interval = Duration::from_secs(state.config.poll_interval_seconds);
    let (ready, lag) = scheduler_readiness(last_tick, interval, now);
    let body = json!({
        "status": if ready { "ready" } else { "starting" },
        "scheduler_lag_seconds": lag,
        "poll_interval_seconds": state.config.poll_interval_seconds,
        "running": lock_running(&state).len(),
        "jobs_dir": state.config.jobs_dir.is_dir(),
        "now": Local::now().to_rfc3339(),
    });
    if ready {
        (StatusCode::OK, Json(body)).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
    }
}

async fn dashboard() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(include_str!("../static/index.html")),
    )
}

async fn get_jobs(State(state): State<Arc<AppState>>) -> ApiResult<Json<Vec<Job>>> {
    Ok(Json(list_jobs(&state.config)?))
}

async fn get_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<Job>> {
    Ok(Json(read_job(&state.config, &name)?))
}

async fn create_job(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateJob>,
) -> ApiResult<(StatusCode, Json<Job>)> {
    let _guard = state.repository_lock.lock().await;
    let path = job_path(&state.config, &payload.name)?;
    if path.exists() {
        return Err(api_error(StatusCode::CONFLICT, "Job already exists."));
    }
    validate_schedule(&payload.datetime)?;
    validate_text_length("description", &payload.description, MAX_DESCRIPTION_BYTES)?;
    validate_text_length("datetime", &payload.datetime, MAX_DATETIME_BYTES)?;
    validate_text_length("action", &payload.action, MAX_ACTION_BYTES)?;
    if payload.action.trim().is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "action cannot be empty.",
        ));
    }
    let status = normalize_status(payload.status.as_deref().unwrap_or("ativo"))?;
    let working_dir = normalize_working_dir(payload.working_dir.as_deref())?;
    let timeout_seconds = normalize_timeout(payload.timeout_seconds.unwrap_or(0))?;
    let job = Job {
        name: payload.name.clone(),
        description: payload.description,
        datetime: payload.datetime.trim().to_owned(),
        action: payload.action,
        log: format!("logs/{}.log", payload.name),
        status,
        last_run: None,
        working_dir,
        timeout_seconds,
        last_result: None,
        last_exit_code: None,
        last_duration_ms: None,
    };
    write_job(&state.config, &job)?;
    Ok((StatusCode::CREATED, Json(job)))
}

async fn update_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(payload): Json<UpdateJob>,
) -> ApiResult<Json<Job>> {
    let _guard = state.repository_lock.lock().await;
    if lock_running(&state).contains(&name) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Cannot edit a job while it is running.",
        ));
    }
    let mut job = read_job(&state.config, &name)?;
    if let Some(description) = payload.description {
        validate_text_length("description", &description, MAX_DESCRIPTION_BYTES)?;
        job.description = description;
    }
    if let Some(datetime) = payload.datetime {
        validate_schedule(&datetime)?;
        validate_text_length("datetime", &datetime, MAX_DATETIME_BYTES)?;
        job.datetime = datetime.trim().to_owned();
        clear_last_run_if_not_current_minute(&mut job);
    }
    if let Some(action) = payload.action {
        validate_text_length("action", &action, MAX_ACTION_BYTES)?;
        if action.trim().is_empty() {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "action cannot be empty.",
            ));
        }
        job.action = action;
    }
    if let Some(working_dir) = payload.working_dir {
        job.working_dir = normalize_working_dir(Some(&working_dir))?;
    }
    if let Some(timeout_seconds) = payload.timeout_seconds {
        job.timeout_seconds = normalize_timeout(timeout_seconds)?;
    }
    if let Some(status) = payload.status {
        let next_status = normalize_status(&status)?;
        if job.status != "ativo" && next_status == "ativo" {
            clear_last_run_if_not_current_minute(&mut job);
        }
        job.status = next_status;
    }
    write_job(&state.config, &job)?;
    Ok(Json(job))
}

async fn set_job_status(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    status: &'static str,
) -> ApiResult<Json<Job>> {
    let _guard = state.repository_lock.lock().await;
    if lock_running(&state).contains(&name) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Cannot change status while the job is running.",
        ));
    }
    let mut job = read_job(&state.config, &name)?;
    if job.status != "ativo" && status == "ativo" {
        clear_last_run_if_not_current_minute(&mut job);
    }
    job.status = status.to_owned();
    write_job(&state.config, &job)?;
    Ok(Json(job))
}

async fn activate_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<Job>> {
    set_job_status(State(state), Path(name), "ativo").await
}

async fn deactivate_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<Job>> {
    set_job_status(State(state), Path(name), "desativo").await
}

async fn delete_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let _guard = state.repository_lock.lock().await;
    if lock_running(&state).contains(&name) {
        return Err(api_error(StatusCode::CONFLICT, "Job is currently running."));
    }
    let path = job_path(&state.config, &name)?;
    fs::remove_file(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => api_error(StatusCode::NOT_FOUND, "Job not found."),
        _ => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot delete job: {error}"),
        ),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn run_job(state: Arc<AppState>, name: &str, origin: RunOrigin) -> ApiResult<()> {
    // Cap the total number of concurrent commands so a burst of schedules cannot
    // exhaust the machine. Scheduled runs that hit the cap are skipped and
    // retried on the next tick.
    let _slot = state.job_slots.clone().try_acquire_owned().map_err(|_| {
        api_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "Too many jobs running at once (limit {}).",
                state.config.max_concurrent_jobs
            ),
        )
    })?;
    let job = {
        let _guard = state.repository_lock.lock().await;
        let mut running = lock_running(&state);
        if running.contains(name) {
            return Err(api_error(StatusCode::CONFLICT, "Job is already running."));
        }
        let mut job = read_job(&state.config, name)?;
        if job.status != "ativo" {
            return Err(api_error(StatusCode::CONFLICT, "Job is not active."));
        }
        if parse_datetime(&job.datetime).is_ok() {
            job.status = "desativo".to_owned();
        }
        job.last_run = Some(Local::now().format("%Y-%m-%d %H:%M").to_string());
        write_job(&state.config, &job)?;
        running.insert(name.to_owned());
        job
    };
    let _running_guard = RunningJobGuard {
        running: state.running_jobs.clone(),
        name: name.to_owned(),
    };

    let outcome = execute_command(&state, &job).await;
    record_outcome(&state, name, &outcome).await;
    state.audit(json!({
        "ts": Local::now().to_rfc3339(),
        "event": "job.run",
        "job": name,
        "origin": origin.as_str(),
        "result": outcome.label(),
        "exit_code": outcome.exit_code,
        "duration_ms": outcome.duration.as_millis().min(u64::MAX as u128) as u64,
    }));

    match outcome.label() {
        "success" => Ok(()),
        "timeout" => Err(api_error(
            StatusCode::GATEWAY_TIMEOUT,
            format!(
                "Job exceeded the {} second command timeout.",
                job.timeout_seconds
                    .unwrap_or(state.config.command_timeout_seconds)
            ),
        )),
        _ => Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            match outcome.error {
                Some(error) => error,
                None => format!(
                    "Job exited with status {}.",
                    outcome.exit_code.unwrap_or(-1)
                ),
            },
        )),
    }
}

/// Persists the result of the last run so clients can show a status badge.
async fn record_outcome(state: &AppState, name: &str, outcome: &RunOutcome) {
    let _guard = state.repository_lock.lock().await;
    let Ok(mut job) = read_job(&state.config, name) else {
        return;
    };
    job.last_result = Some(outcome.label().to_owned());
    job.last_exit_code = outcome.exit_code;
    job.last_duration_ms = Some(outcome.duration.as_millis().min(u64::MAX as u128) as u64);
    if let Err(error) = write_job(&state.config, &job) {
        eprintln!("Cannot record run outcome for '{name}': {}", error.1);
    }
}

async fn execute_command(state: &AppState, job: &Job) -> RunOutcome {
    let config = &state.config;
    let started = Instant::now();
    let mut outcome = RunOutcome {
        exit_code: None,
        timed_out: false,
        duration: Duration::ZERO,
        error: None,
    };

    let finish = |outcome: &mut RunOutcome, error: Option<String>| {
        outcome.error = error;
        outcome.duration = started.elapsed();
    };

    if let Err(error) = fs::create_dir_all(&config.logs_dir) {
        finish(
            &mut outcome,
            Some(format!("Cannot create logs directory: {error}")),
        );
        return outcome;
    }
    rotate_log_if_needed(config, &job.name);
    let log_path = config.logs_dir.join(format!("{}.log", job.name));
    let mut output = match OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
    {
        Ok(file) => file,
        Err(error) => {
            finish(&mut outcome, Some(format!("Cannot open job log: {error}")));
            return outcome;
        }
    };
    let _ = fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600));
    let error_output = match output.try_clone() {
        Ok(file) => file,
        Err(error) => {
            finish(
                &mut outcome,
                Some(format!("Cannot open job log for stderr: {error}")),
            );
            return outcome;
        }
    };

    let _ = writeln!(
        output,
        "\n───── DataTask · {} · iniciando \"{}\" ─────",
        Local::now().format("%d/%m/%Y %H:%M:%S"),
        job.name
    );
    let _ = output.flush();

    let timeout_seconds = job
        .timeout_seconds
        .unwrap_or(config.command_timeout_seconds);
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(&job.action)
        .kill_on_drop(true)
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error_output));
    // Run the command in its own process group so a timeout can terminate the
    // whole tree (pipelines, background children), not just the `/bin/sh` parent.
    command.process_group(0);
    if let Some(working_dir) = &job.working_dir {
        command.current_dir(working_dir);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            finish(
                &mut outcome,
                Some(format!("Cannot start job command: {error}")),
            );
            append_log_footer(config, job, &outcome);
            return outcome;
        }
    };
    let child_pid = child.id();
    if let Some(pid) = child_pid {
        state
            .process_groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(pid);
    }
    match time::timeout(Duration::from_secs(timeout_seconds), child.wait()).await {
        Ok(Ok(status)) => outcome.exit_code = status.code(),
        Ok(Err(error)) => {
            finish(
                &mut outcome,
                Some(format!("Cannot wait for job command: {error}")),
            );
        }
        Err(_) => {
            if let Some(pid) = child_pid {
                kill_process_group(pid);
            }
            let _ = child.wait().await;
            outcome.timed_out = true;
        }
    }
    if let Some(pid) = child_pid {
        state
            .process_groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&pid);
    }
    outcome.duration = started.elapsed();
    append_log_footer(config, job, &outcome);
    outcome
}

fn rotate_log_if_needed(config: &Config, name: &str) {
    if config.log_max_bytes == 0 {
        return;
    }
    let path = config.logs_dir.join(format!("{name}.log"));
    let Ok(metadata) = fs::metadata(&path) else {
        return;
    };
    if metadata.len() < config.log_max_bytes {
        return;
    }
    let rotated = config.logs_dir.join(format!("{name}.log.1"));
    let _ = fs::remove_file(&rotated);
    if let Err(error) = fs::rename(&path, &rotated) {
        eprintln!("Cannot rotate log for '{name}': {error}");
    }
}

fn kill_process_group(pid: u32) {
    // The child was spawned with process_group(0), so its pid is also the
    // process-group id. A negative pid signals the entire group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

fn append_log_footer(config: &Config, job: &Job, outcome: &RunOutcome) {
    let log_path = config.logs_dir.join(format!("{}.log", job.name));
    let Ok(mut file) = OpenOptions::new().append(true).open(&log_path) else {
        return;
    };
    let elapsed = outcome.duration.as_secs_f64();
    let summary = match outcome.label() {
        "success" => "concluído".to_owned(),
        "timeout" => "interrompido por tempo limite".to_owned(),
        _ => match outcome.exit_code {
            Some(code) => format!("falhou com código {code}"),
            None => "falhou".to_owned(),
        },
    };
    let _ = writeln!(
        file,
        "───── DataTask · {} · {summary} em {elapsed:.2}s ─────\n",
        Local::now().format("%d/%m/%Y %H:%M:%S")
    );
    let _ = file.flush();
}

async fn run_job_endpoint(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    // Validate before detaching so the client gets an immediate, accurate error.
    {
        let _guard = state.repository_lock.lock().await;
        let job = read_job(&state.config, &name)?;
        if job.status != "ativo" {
            return Err(api_error(StatusCode::CONFLICT, "Job is not active."));
        }
        if lock_running(&state).contains(&name) {
            return Err(api_error(StatusCode::CONFLICT, "Job is already running."));
        }
        if state.job_slots.available_permits() == 0 {
            return Err(api_error(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Too many jobs running at once (limit {}).",
                    state.config.max_concurrent_jobs
                ),
            ));
        }
    }
    let spawned_name = name.clone();
    tokio::spawn(async move {
        if let Err(error) = run_job(state, &spawned_name, RunOrigin::Manual).await {
            eprintln!("Manual run of '{spawned_name}' failed: {}", error.1);
        }
    });
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct LogQuery {
    download: Option<String>,
}

#[derive(Deserialize)]
struct LogTailQuery {
    offset: Option<u64>,
}

async fn get_job_logs(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Response> {
    read_job(&state.config, &name)?;
    let path = state.config.logs_dir.join(format!("{name}.log"));
    let mut file = File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => api_error(StatusCode::NOT_FOUND, "Log not found."),
        _ => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot read log: {error}"),
        ),
    })?;
    let length = file
        .metadata()
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot inspect log: {error}"),
            )
        })?
        .len();
    let start = length.saturating_sub(LOG_RESPONSE_MAX_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot seek log: {error}"),
        )
    })?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot read log: {error}"),
        )
    })?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if query
        .download
        .as_deref()
        .is_some_and(|value| value != "0" && value != "false")
    {
        let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}.log\""))
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "Invalid log filename."))?;
        headers.insert(header::CONTENT_DISPOSITION, disposition);
    }
    Ok((headers, contents).into_response())
}

/// Incremental log read used for live tailing in the panel.
async fn tail_job_logs(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<LogTailQuery>,
) -> ApiResult<Json<Value>> {
    read_job(&state.config, &name)?;
    let path = state.config.logs_dir.join(format!("{name}.log"));
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Json(json!({
                "exists": false,
                "content": "",
                "offset": 0,
                "size": 0
            })));
        }
        Err(error) => {
            return Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot read log: {error}"),
            ));
        }
    };
    let size = file
        .metadata()
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot inspect log: {error}"),
            )
        })?
        .len();
    // Without an offset, return the most recent chunk and place the cursor at EOF.
    let (start, mut limit) = match query.offset {
        None => (size.saturating_sub(LOG_RESPONSE_MAX_BYTES), size),
        Some(offset) if offset > size => (0, size),
        Some(offset) => (offset, size),
    };
    if start < limit && limit - start > LOG_CHUNK_MAX_BYTES {
        limit = start + LOG_CHUNK_MAX_BYTES;
    }
    file.seek(SeekFrom::Start(start)).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot seek log: {error}"),
        )
    })?;
    let mut contents = Vec::with_capacity((limit - start) as usize);
    file.take(limit - start)
        .read_to_end(&mut contents)
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot read log: {error}"),
            )
        })?;
    Ok(Json(json!({
        "exists": true,
        "content": String::from_utf8_lossy(&contents),
        "offset": start + contents.len() as u64,
        "size": size
    })))
}

async fn clear_job_logs(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    read_job(&state.config, &name)?;
    let path = state.config.logs_dir.join(format!("{name}.log"));
    match fs::remove_file(path) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot clear log: {error}"),
        )),
    }
}

async fn get_info(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let jobs = list_jobs(&state.config)?;
    let mut running: Vec<String> = lock_running(&state).iter().cloned().collect();
    running.sort();
    Ok(Json(json!({
        "name": "DataTask",
        "version": env!("CARGO_PKG_VERSION"),
        "bind_address": state.config.bind_address,
        "port": state.config.port,
        "poll_interval_seconds": state.config.poll_interval_seconds,
        "command_timeout_seconds": state.config.command_timeout_seconds,
        "max_concurrent_jobs": state.config.max_concurrent_jobs,
        "log_max_bytes": state.config.log_max_bytes,
        "rate_limit_per_minute": state.config.rate_limit_per_minute,
        "auth_failure_limit": state.config.auth_failure_limit,
        "auth_lockout_seconds": state.config.auth_lockout_seconds,
        "jobs_dir": state.config.jobs_dir.display().to_string(),
        "logs_dir": state.config.logs_dir.display().to_string(),
        "job_count": jobs.len(),
        "active_count": jobs.iter().filter(|job| job.status == "ativo").count(),
        "running": running,
        "now": Local::now().to_rfc3339()
    })))
}

async fn stylesheet() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../static/app.css"),
    )
}

async fn script() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../static/app.js"),
    )
}

async fn scheduler(state: Arc<AppState>) {
    let interval = Duration::from_secs(state.config.poll_interval_seconds);
    let mut ticker = time::interval(interval);
    loop {
        ticker.tick().await;
        *state
            .last_tick
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
        let now = Local::now();
        let entries = match fs::read_dir(&state.config.jobs_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                eprintln!("Scheduler cannot list jobs: {error}");
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("Scheduler cannot read a job file: {error}");
                    continue;
                }
            };
            if entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let path = entry.path();
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                eprintln!("Scheduler found a job with an invalid filename.");
                continue;
            };
            let job = match read_job(&state.config, name) {
                Ok(job) => job,
                Err(error) => {
                    eprintln!("Scheduler cannot read job '{name}': {}", error.1);
                    continue;
                }
            };
            if job.status != "ativo" {
                continue;
            }
            let current_minute = now.format("%Y-%m-%d %H:%M").to_string();
            if job.last_run.as_deref() == Some(&current_minute) {
                continue;
            }
            match schedule_matches(&job.datetime, &now) {
                Ok(true) => {
                    // Run due jobs concurrently so a slow task never delays the others.
                    let state = state.clone();
                    let job_name = job.name.clone();
                    tokio::spawn(async move {
                        if let Err(error) = run_job(state, &job_name, RunOrigin::Scheduled).await {
                            eprintln!("Job '{job_name}' failed: {}", error.1);
                        }
                    });
                }
                Ok(false) => {}
                Err(error) => eprintln!("Invalid schedule for '{}': {}", job.name, error.1),
            }
        }
    }
}

fn prepare_storage(config: &Config) -> std::io::Result<()> {
    for directory in [&config.jobs_dir, &config.logs_dir] {
        fs::create_dir_all(directory)?;
        match fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                eprintln!(
                    "warning: {} is a symlink; make sure its target is private.",
                    directory.display()
                );
            }
            Ok(_) => {
                if let Err(error) =
                    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                {
                    eprintln!(
                        "warning: cannot restrict permissions of {}: {error}",
                        directory.display()
                    );
                }
            }
            Err(error) => {
                eprintln!(
                    "warning: cannot inspect permissions of {}: {error}",
                    directory.display()
                );
            }
        }
        // Tighten files created by older versions or by hand.
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(metadata) = fs::symlink_metadata(&path)
                && metadata.file_type().is_file()
            {
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
        }
    }

    if let Some(audit) = &config.audit_log_file {
        if let Some(parent) = audit.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
        if audit.is_file() {
            let _ = fs::set_permissions(audit, fs::Permissions::from_mode(0o600));
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

async fn wait_for_running_jobs(state: &AppState) {
    let deadline = Instant::now() + Duration::from_secs(SHUTDOWN_GRACE_SECONDS);
    while Instant::now() < deadline {
        if lock_running(state).is_empty() {
            return;
        }
        time::sleep(Duration::from_millis(100)).await;
    }
    let groups: Vec<u32> = state
        .process_groups
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .copied()
        .collect();
    if !groups.is_empty() {
        eprintln!(
            "Shutdown grace period elapsed; terminating {} job process group(s).",
            groups.len()
        );
        for pid in groups {
            kill_process_group(pid);
        }
        // Give the executor tasks a moment to observe the exit and write their
        // log footers before the runtime is dropped.
        let drain_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < drain_deadline && !lock_running(state).is_empty() {
            time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Files created by the scheduler (jobs, logs, rotated logs) must never be
    // world-readable: job actions and output frequently contain secrets.
    unsafe {
        libc::umask(0o077);
    }

    let config_path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    let (config, token) = load_config(&config_path)?;
    let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    drop(token);
    prepare_storage(&config)?;

    if !matches!(
        config.bind_address.as_str(),
        "127.0.0.1" | "::1" | "localhost"
    ) {
        eprintln!(
            "warning: binding to {} exposes the plaintext HTTP API; put it behind a TLS reverse proxy.",
            config.bind_address
        );
    }

    let config = Arc::new(config);
    let guard = Arc::new(AccessGuard::new(RateLimits {
        per_minute: config.rate_limit_per_minute,
        failure_limit: config.auth_failure_limit,
        failure_window: Duration::from_secs(config.auth_failure_window_seconds),
        lockout: Duration::from_secs(config.auth_lockout_seconds),
    }));
    let audit = config
        .audit_log_file
        .clone()
        .map(|path| Arc::new(AuditLog::new(path)));
    let state = Arc::new(AppState {
        config: config.clone(),
        repository_lock: Arc::new(Mutex::new(())),
        running_jobs: Arc::new(StdMutex::new(HashSet::new())),
        process_groups: Arc::new(StdMutex::new(HashSet::new())),
        job_slots: Arc::new(Semaphore::new(config.max_concurrent_jobs)),
        token_digest: digest,
        guard,
        audit,
        last_tick: Arc::new(StdMutex::new(None)),
    });
    let scheduler_handle = tokio::spawn(scheduler(state.clone()));

    let api: Router = Router::new()
        .route("/api/info", get(get_info))
        .route("/api/jobs", get(get_jobs).post(create_job))
        .route(
            "/api/jobs/{name}",
            get(get_job).patch(update_job).delete(delete_job),
        )
        .route("/api/jobs/{name}/run", post(run_job_endpoint))
        .route("/api/jobs/{name}/activate", post(activate_job))
        .route("/api/jobs/{name}/deactivate", post(deactivate_job))
        .route(
            "/api/jobs/{name}/logs",
            get(get_job_logs).delete(clear_job_logs),
        )
        .route("/api/jobs/{name}/logs/tail", get(tail_job_logs))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state.clone());
    let readiness: Router = Router::new()
        .route("/readyz", get(readyz))
        .with_state(state.clone());
    let app: Router = Router::new()
        .route("/", get(dashboard))
        .route("/health", get(health))
        .route("/app.css", get(stylesheet))
        .route("/app.js", get(script))
        .route("/assets/app.css", get(stylesheet))
        .route("/assets/app.js", get(script))
        .merge(readiness)
        .merge(api)
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn_with_state(state.clone(), api_guard));

    let address = format!("{}:{}", config.bind_address, config.port);
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("DataTask API listening on http://{address}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // Stop scheduling new work, then give in-flight commands a short grace
    // period so their logs get a proper footer.
    scheduler_handle.abort();
    let _ = scheduler_handle.await;
    wait_for_running_jobs(&state).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_supported_one_time_formats() {
        assert!(parse_datetime("29 10 2026 15 10").is_ok());
        assert!(parse_datetime("2026-10-29 15:10").is_ok());
        assert!(parse_datetime("2026-10-29 15:10:30").is_err());
        assert!(validate_schedule("29 10 2026 15 10").is_ok());
        assert!(validate_schedule("* * * * *").is_ok());
        assert!(validate_schedule("60 * * * *").is_err());
    }

    #[test]
    fn matches_minute_hour_and_weekday_schedules() {
        let monday = Local.with_ymd_and_hms(2026, 10, 26, 8, 30, 0).unwrap();
        assert!(cron_matches("30 8 * * 1-5", &monday).unwrap());
        assert!(cron_matches("*/15 * * * *", &monday).unwrap());
        assert!(!cron_matches("0 * * * *", &monday).unwrap());
    }

    #[test]
    fn validates_status_and_job_names() {
        assert_eq!(normalize_status("pending").unwrap(), "ativo");
        assert_eq!(normalize_status("desativo").unwrap(), "desativo");
        assert!(normalize_status("unknown").is_err());
        assert!(valid_job_name("job_1-ok"));
        assert!(!valid_job_name("../outside"));
    }

    #[test]
    fn validates_explicit_timeouts() {
        assert_eq!(normalize_timeout(0).unwrap(), None);
        assert_eq!(normalize_timeout(60).unwrap(), Some(60));
        assert!(normalize_timeout(MAX_JOB_TIMEOUT_SECONDS + 1).is_err());
        let existing = std::env::temp_dir();
        assert!(normalize_working_dir(existing.to_str()).unwrap().is_some());
        assert_eq!(normalize_working_dir(Some("")).unwrap(), None);
        let missing = existing.join(format!("datatask-missing-{}", std::process::id()));
        assert!(!missing.exists());
        assert!(normalize_working_dir(missing.to_str()).is_err());
    }

    #[test]
    fn rejects_weak_or_placeholder_tokens() {
        assert!(validate_token("short").is_err());
        assert!(validate_token(&"a".repeat(64)).is_err());
        assert!(validate_token("substitua-por-um-segredo-com-pelo-menos-32-caracteres").is_err());
        assert!(validate_token("0123456789abcdef0123456789abcdef  ").is_err());
        assert!(validate_token(&"0123456789abcdef".repeat(4)).is_ok());
    }

    #[test]
    fn enforces_text_field_limits() {
        assert!(validate_text_length("action", "echo ok", MAX_ACTION_BYTES).is_ok());
        assert!(
            validate_text_length(
                "action",
                &"a".repeat(MAX_ACTION_BYTES + 1),
                MAX_ACTION_BYTES
            )
            .is_err()
        );
    }

    #[test]
    fn rate_limits_per_window_and_resets() {
        let guard = AccessGuard::new(RateLimits {
            per_minute: 2,
            failure_limit: 0,
            failure_window: Duration::from_secs(60),
            lockout: Duration::from_secs(5),
        });
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let start = Instant::now();

        assert!(guard.check(ip, start).is_ok());
        assert!(guard.check(ip, start).is_ok());
        assert!(matches!(
            guard.check(ip, start),
            Err(Rejection::RateLimited(_))
        ));
        assert!(guard.check(ip, start + Duration::from_secs(61)).is_ok());
    }

    #[test]
    fn failed_auth_locks_out_and_success_resets() {
        let guard = AccessGuard::new(RateLimits {
            per_minute: 100,
            failure_limit: 2,
            failure_window: Duration::from_secs(60),
            lockout: Duration::from_secs(5),
        });
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let start = Instant::now();

        assert_eq!(guard.record_failure(ip, start), None);
        assert_eq!(
            guard.record_failure(ip, start),
            Some(Duration::from_secs(5))
        );
        assert!(matches!(
            guard.check(ip, start + Duration::from_secs(1)),
            Err(Rejection::AuthLocked(_))
        ));
        assert!(guard.check(ip, start + Duration::from_secs(6)).is_ok());
        guard.record_success(ip);
        assert_eq!(
            guard.record_failure(ip, start + Duration::from_secs(7)),
            None
        );
    }

    #[test]
    fn readiness_requires_a_recent_scheduler_tick() {
        let now = Instant::now();
        let interval = Duration::from_secs(1);
        assert!(!scheduler_readiness(None, interval, now).0);
        assert!(scheduler_readiness(Some(now), interval, now).0);
        assert!(!scheduler_readiness(Some(now - Duration::from_secs(9)), interval, now).0);
        assert_eq!(job_from_path("/api/jobs/task/logs"), Some("task"));
        assert_eq!(job_from_path("/api/jobs"), None);
    }
}
