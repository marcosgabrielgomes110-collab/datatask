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
use std::fmt::Write as _;
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::fd::AsRawFd,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
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
    /// Assert that a trusted reverse proxy terminates TLS in front of this
    /// process. Required when `bind_address` is not loopback.
    #[serde(default)]
    tls_terminated_by_proxy: bool,
    /// Addresses allowed to supply `X-Forwarded-For`/`X-Forwarded-Proto`.
    /// Loopback is always trusted; remote proxies must be listed explicitly.
    #[serde(default)]
    trusted_proxy_ips: Vec<IpAddr>,
    /// Native TLS certificate and key. Set both or neither; they are mutually
    /// exclusive with `tls_terminated_by_proxy`.
    #[serde(default)]
    tls_cert_file: Option<PathBuf>,
    #[serde(default)]
    tls_key_file: Option<PathBuf>,
    #[serde(default)]
    api_token: Option<String>,
    #[serde(default)]
    api_token_file: Option<PathBuf>,
    jobs_dir: PathBuf,
    logs_dir: PathBuf,
    poll_interval_seconds: u64,
    command_timeout_seconds: u64,
    #[serde(default = "default_notification_timeout_seconds")]
    notification_timeout_seconds: u64,
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
    /// JSONL execution history. Empty string disables it; omitted defaults to `<logs_dir>/history.jsonl`.
    #[serde(default)]
    history_file: Option<PathBuf>,
    /// Directory for periodic JSON backups. Empty/omitted disables backups.
    #[serde(default)]
    backup_dir: Option<PathBuf>,
    #[serde(default = "default_backup_interval_seconds")]
    backup_interval_seconds: u64,
    #[serde(default = "default_backup_retention")]
    backup_retention: u32,
    /// Optional offsite copy destination: SSH target (`user@host:/dir`) or S3 URI.
    #[serde(default)]
    backup_ssh_target: Option<String>,
    #[serde(default = "default_ssh_port")]
    backup_ssh_port: u16,
    #[serde(default)]
    backup_s3_uri: Option<String>,
    /// GPG passphrase file used to encrypt the uploaded artifact (600).
    #[serde(default)]
    backup_encryption_key_file: Option<PathBuf>,
    #[serde(default = "default_backup_remote_timeout_seconds")]
    backup_remote_timeout_seconds: u64,
    /// Periodic remote checksum verification. `0` disables the periodic check.
    #[serde(default = "default_backup_verify_interval_seconds")]
    backup_verify_interval_seconds: u64,
    /// Optional webhook called when remote verification fails or has no backups.
    #[serde(default)]
    backup_alert_url: Option<String>,
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

fn default_notification_timeout_seconds() -> u64 {
    10
}

fn default_retry_delay_seconds() -> u64 {
    5
}

fn default_backup_interval_seconds() -> u64 {
    86_400
}

fn default_backup_retention() -> u32 {
    7
}

fn default_ssh_port() -> u16 {
    22
}

fn default_backup_remote_timeout_seconds() -> u64 {
    60
}

fn default_backup_verify_interval_seconds() -> u64 {
    86_400
}

fn default_job_version() -> u64 {
    1
}

fn bind_is_loopback(value: &str) -> bool {
    value.eq_ignore_ascii_case("localhost")
        || value
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn native_tls(config: &Config) -> bool {
    config.tls_cert_file.is_some() && config.tls_key_file.is_some()
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    repository_lock: Arc<Mutex<()>>,
    job_cache: Arc<JobCache>,
    running_jobs: Arc<StdMutex<HashSet<String>>>,
    /// PIDs that are also process-group leaders, so shutdown can terminate
    /// commands that outlive the grace period.
    process_groups: Arc<StdMutex<HashSet<u32>>>,
    job_slots: Arc<Semaphore>,
    backup_lock: Arc<Mutex<()>>,
    token_digest: [u8; 32],
    guard: Arc<AccessGuard>,
    audit: Option<Arc<AuditLog>>,
    history: Option<Arc<HistoryLog>>,
    last_tick: Arc<StdMutex<Option<Instant>>>,
    last_backup_verify: Arc<StdMutex<Option<BackupVerifySummary>>>,
    last_backup_alert: Arc<StdMutex<Option<String>>>,
    verify_history: Arc<VerifyHistoryLog>,
    backup_health: Arc<StdMutex<BackupHealth>>,
    metrics: Arc<Metrics>,
    http_client: reqwest::Client,
}

impl AppState {
    fn audit(&self, event: Value) {
        if let Some(audit) = &self.audit {
            audit.write(event);
        }
    }

    fn record_history(&self, record: RunRecord) {
        if let Some(history) = &self.history {
            history.append(&record);
        }
    }

    fn record_backup_event(&self, kind: BackupEventKind, ok: bool, detail: Option<String>) {
        let event = BackupEvent {
            timestamp: Local::now().to_rfc3339(),
            ok,
            detail,
        };
        let mut health = self
            .backup_health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match kind {
            BackupEventKind::Upload => health.last_upload = Some(event),
            BackupEventKind::Prune => health.last_prune = Some(event),
            BackupEventKind::Alert => health.last_alert = Some(event),
        }
    }
}

const DURATION_BUCKETS: [f64; 7] = [0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0];

/// Holds an exclusive `flock` for the data directory for the whole process
/// lifetime, preventing two schedulers from running the same jobs.
struct InstanceLock {
    file: File,
}

impl InstanceLock {
    fn acquire(path: &FsPath) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "Another DataTask instance is already using the jobs in {}. Stop it or use a different jobs_dir.",
                        path.parent()
                            .map(|parent| parent.display().to_string())
                            .unwrap_or_else(|| path.display().to_string())
                    ),
                ));
            }
            return Err(io::Error::new(
                error.kind(),
                format!("Cannot lock {}: {error}", path.display()),
            ));
        }
        Ok(Self { file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Process-local counters exposed at `/metrics`. Metrics are intentionally
/// aggregate: job names and command output never appear in the exposition.
struct Metrics {
    started: Instant,
    runs_success: AtomicU64,
    runs_error: AtomicU64,
    runs_timeout: AtomicU64,
    duration_count: AtomicU64,
    duration_sum_micros: AtomicU64,
    buckets: [AtomicU64; DURATION_BUCKETS.len()],
}

impl Metrics {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            runs_success: AtomicU64::new(0),
            runs_error: AtomicU64::new(0),
            runs_timeout: AtomicU64::new(0),
            duration_count: AtomicU64::new(0),
            duration_sum_micros: AtomicU64::new(0),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn observe(&self, outcome: &RunOutcome) {
        match outcome.label() {
            "success" => increment(&self.runs_success),
            "timeout" => increment(&self.runs_timeout),
            _ => increment(&self.runs_error),
        }
        let seconds = outcome.duration.as_secs_f64();
        for (index, boundary) in DURATION_BUCKETS.iter().enumerate() {
            if seconds <= *boundary {
                increment(&self.buckets[index]);
            }
        }
        increment(&self.duration_count);
        self.duration_sum_micros.fetch_add(
            outcome.duration.as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    fn render(
        &self,
        jobs_total: usize,
        jobs_active: usize,
        jobs_running: usize,
        scheduler_ready: bool,
        scheduler_lag: Option<f64>,
    ) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "# HELP datatask_up Whether the process is serving metrics."
        );
        let _ = writeln!(output, "# TYPE datatask_up gauge");
        let _ = writeln!(output, "datatask_up 1");
        let _ = writeln!(
            output,
            "# HELP datatask_build_info Build information for DataTask."
        );
        let _ = writeln!(output, "# TYPE datatask_build_info gauge");
        let _ = writeln!(
            output,
            "datatask_build_info{{version=\"{}\"}} 1",
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(
            output,
            "# HELP datatask_uptime_seconds Seconds since the process started."
        );
        let _ = writeln!(output, "# TYPE datatask_uptime_seconds gauge");
        let _ = writeln!(
            output,
            "datatask_uptime_seconds {:.3}",
            self.started.elapsed().as_secs_f64()
        );
        let _ = writeln!(output, "# HELP datatask_jobs_total Configured jobs.");
        let _ = writeln!(output, "# TYPE datatask_jobs_total gauge");
        let _ = writeln!(output, "datatask_jobs_total {jobs_total}");
        let _ = writeln!(output, "# HELP datatask_jobs_active Active jobs.");
        let _ = writeln!(output, "# TYPE datatask_jobs_active gauge");
        let _ = writeln!(output, "datatask_jobs_active {jobs_active}");
        let _ = writeln!(
            output,
            "# HELP datatask_jobs_running Jobs executing right now."
        );
        let _ = writeln!(output, "# TYPE datatask_jobs_running gauge");
        let _ = writeln!(output, "datatask_jobs_running {jobs_running}");
        let _ = writeln!(
            output,
            "# HELP datatask_scheduler_ready Whether the scheduler heartbeat is recent."
        );
        let _ = writeln!(output, "# TYPE datatask_scheduler_ready gauge");
        let _ = writeln!(
            output,
            "datatask_scheduler_ready {}",
            u8::from(scheduler_ready)
        );
        let _ = writeln!(
            output,
            "# HELP datatask_scheduler_lag_seconds Time since the last scheduler tick."
        );
        let _ = writeln!(output, "# TYPE datatask_scheduler_lag_seconds gauge");
        let _ = writeln!(
            output,
            "datatask_scheduler_lag_seconds {}",
            scheduler_lag
                .map(|lag| format!("{lag:.6}"))
                .unwrap_or_else(|| "NaN".to_owned())
        );
        let _ = writeln!(
            output,
            "# HELP datatask_job_runs_total Job executions by result."
        );
        let _ = writeln!(output, "# TYPE datatask_job_runs_total counter");
        for (result, counter) in [
            ("success", &self.runs_success),
            ("error", &self.runs_error),
            ("timeout", &self.runs_timeout),
        ] {
            let _ = writeln!(
                output,
                "datatask_job_runs_total{{result=\"{result}\"}} {}",
                counter.load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            output,
            "# HELP datatask_job_duration_seconds Duration of job executions."
        );
        let _ = writeln!(output, "# TYPE datatask_job_duration_seconds histogram");
        for (boundary, counter) in DURATION_BUCKETS.iter().zip(self.buckets.iter()) {
            let _ = writeln!(
                output,
                "datatask_job_duration_seconds_bucket{{le=\"{boundary}\"}} {}",
                counter.load(Ordering::Relaxed)
            );
        }
        let count = self.duration_count.load(Ordering::Relaxed);
        let _ = writeln!(
            output,
            "datatask_job_duration_seconds_bucket{{le=\"+Inf\"}} {count}"
        );
        let _ = writeln!(
            output,
            "datatask_job_duration_seconds_sum {:.6}",
            self.duration_sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0
        );
        let _ = writeln!(output, "datatask_job_duration_seconds_count {count}");
        output
    }
}

fn increment(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
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

fn peer_ip(request: &Request<Body>) -> IpAddr {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

fn is_trusted_proxy(peer: IpAddr, trusted: &[IpAddr]) -> bool {
    peer.is_loopback() || trusted.contains(&peer)
}

const HISTORY_MAX_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RunRecord {
    timestamp: String,
    job: String,
    origin: String,
    attempt: u32,
    max_attempts: u32,
    result: String,
    exit_code: Option<i32>,
    duration_ms: u64,
    timed_out: bool,
}

/// Append-only JSONL history for completed run attempts.
struct HistoryLog {
    path: PathBuf,
    lock: StdMutex<()>,
}

impl HistoryLog {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: StdMutex::new(()),
        }
    }

    fn append(&self, record: &RunRecord) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(parent) = self.path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            eprintln!(
                "Cannot create history directory {}: {error}",
                parent.display()
            );
            return;
        }
        if let Ok(metadata) = fs::metadata(&self.path)
            && metadata.len() >= HISTORY_MAX_BYTES
        {
            let mut rotated = self.path.as_os_str().to_owned();
            rotated.push(".1");
            let rotated = PathBuf::from(rotated);
            let _ = fs::remove_file(&rotated);
            if let Err(error) = fs::rename(&self.path, &rotated) {
                eprintln!("Cannot rotate history log: {error}");
            }
        }
        let line = match serde_json::to_string(record) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("Cannot serialize history entry: {error}");
                return;
            }
        };
        match OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
        {
            Ok(mut file) => {
                let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
                if let Err(error) = writeln!(file, "{line}") {
                    eprintln!("Cannot write history: {error}");
                    return;
                }
                let _ = file.sync_data();
            }
            Err(error) => eprintln!("Cannot open history file {}: {error}", self.path.display()),
        }
    }

    fn read(&self) -> Vec<RunRecord> {
        let mut records = Vec::new();
        let mut rotated = self.path.as_os_str().to_owned();
        rotated.push(".1");
        read_history_file(&PathBuf::from(rotated), &mut records);
        read_history_file(&self.path, &mut records);
        records
    }
}

fn read_history_file(path: &FsPath, records: &mut Vec<RunRecord>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        match serde_json::from_str::<RunRecord>(line) {
            Ok(record) => records.push(record),
            Err(error) => eprintln!("Skipping invalid history entry: {error}"),
        }
    }
}

const VERIFY_HISTORY_MAX_BYTES: u64 = 5 * 1024 * 1024;

struct VerifyHistoryLog {
    path: PathBuf,
    lock: StdMutex<()>,
}

impl VerifyHistoryLog {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: StdMutex::new(()),
        }
    }

    fn append(&self, summary: &BackupVerifySummary) {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(parent) = self.path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            eprintln!(
                "Cannot create backup verification history directory {}: {error}",
                parent.display()
            );
            return;
        }
        if let Ok(metadata) = fs::metadata(&self.path)
            && metadata.len() >= VERIFY_HISTORY_MAX_BYTES
        {
            let mut rotated = self.path.as_os_str().to_owned();
            rotated.push(".1");
            let rotated = PathBuf::from(rotated);
            let _ = fs::remove_file(&rotated);
            if let Err(error) = fs::rename(&self.path, &rotated) {
                eprintln!("Cannot rotate backup verification history: {error}");
            }
        }
        let line = match serde_json::to_string(summary) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("Cannot serialize backup verification history: {error}");
                return;
            }
        };
        match OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
        {
            Ok(mut file) => {
                let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
                if let Err(error) = writeln!(file, "{line}") {
                    eprintln!("Cannot write backup verification history: {error}");
                    return;
                }
                let _ = file.sync_data();
            }
            Err(error) => eprintln!(
                "Cannot open backup verification history {}: {error}",
                self.path.display()
            ),
        }
    }

    fn read(&self) -> Vec<BackupVerifySummary> {
        let mut entries = Vec::new();
        let mut rotated = self.path.as_os_str().to_owned();
        rotated.push(".1");
        read_verify_history_file(&PathBuf::from(rotated), &mut entries);
        read_verify_history_file(&self.path, &mut entries);
        entries
    }
}

fn read_verify_history_file(path: &FsPath, entries: &mut Vec<BackupVerifySummary>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        match serde_json::from_str::<BackupVerifySummary>(line) {
            Ok(summary) => entries.push(summary),
            Err(error) => eprintln!("Skipping invalid backup verification entry: {error}"),
        }
    }
}

fn filter_history(
    records: Vec<RunRecord>,
    job: Option<&str>,
    result: Option<&str>,
    limit: usize,
    offset: usize,
) -> (Vec<RunRecord>, usize) {
    let filtered: Vec<RunRecord> = records
        .into_iter()
        .rev()
        .filter(|record| job.is_none_or(|job| record.job == job))
        .filter(|record| result.is_none_or(|result| record.result == result))
        .collect();
    let total = filtered.len();
    let items = filtered.into_iter().skip(offset).take(limit).collect();
    (items, total)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn write_backup(dir: &FsPath, retention: u32, jobs: &[Job]) -> Result<(PathBuf, String), String> {
    fs::create_dir_all(dir)
        .map_err(|error| format!("Cannot create backup dir {}: {error}", dir.display()))?;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    let portable: Vec<PortableJob> = jobs.iter().map(PortableJob::from).collect();
    let document = json!({
        "format": "datatask.jobs",
        "version": 1,
        "exported_at": Local::now().to_rfc3339(),
        "jobs": portable,
    });
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("Cannot serialize backup: {error}"))?;
    serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("Generated backup is invalid: {error}"))?;
    let checksum = sha256_hex(text.as_bytes());

    let filename = format!(
        "datatask-jobs-{}.json",
        Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = dir.join(&filename);
    let temporary = dir.join(format!(".{filename}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| format!("Cannot write backup: {error}"))?;
    let _ = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600));
    file.write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("Cannot write backup: {error}"))?;
    drop(file);
    fs::rename(&temporary, &path).map_err(|error| format!("Cannot finalize backup: {error}"))?;

    let checksum_path = dir.join(format!("{filename}.sha256"));
    let checksum_temporary = dir.join(format!(".{filename}.sha256.tmp"));
    let mut checksum_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&checksum_temporary)
        .map_err(|error| format!("Cannot write backup checksum: {error}"))?;
    let _ = fs::set_permissions(&checksum_temporary, fs::Permissions::from_mode(0o600));
    checksum_file
        .write_all(format!("{checksum}  {filename}\n").as_bytes())
        .and_then(|_| checksum_file.sync_all())
        .map_err(|error| format!("Cannot write backup checksum: {error}"))?;
    drop(checksum_file);
    fs::rename(&checksum_temporary, &checksum_path)
        .map_err(|error| format!("Cannot finalize backup checksum: {error}"))?;

    prune_backups(dir, retention)?;
    Ok((path, checksum))
}

fn prune_backups(dir: &FsPath, retention: u32) -> Result<(), String> {
    if retention == 0 {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Cannot list backups: {error}")),
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("datatask-jobs-") && name.ends_with(".json"))
        })
        .collect();
    files.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    let remove = files.len().saturating_sub(retention as usize);
    for path in files.into_iter().take(remove) {
        if let Err(error) = fs::remove_file(&path) {
            eprintln!("Cannot remove old backup {}: {error}", path.display());
        }
        let mut checksum = path.as_os_str().to_owned();
        checksum.push(".sha256");
        let _ = fs::remove_file(PathBuf::from(checksum));
    }
    Ok(())
}

enum RemoteTarget {
    Ssh {
        host: String,
        dir: String,
        port: u16,
    },
    S3 {
        base: String,
    },
}

impl RemoteTarget {
    fn label(&self) -> &'static str {
        match self {
            Self::Ssh { .. } => "ssh",
            Self::S3 { .. } => "s3",
        }
    }
}

fn split_ssh_target(target: &str) -> Option<(&str, &str)> {
    let (host, dir) = target.rsplit_once(':')?;
    let host = host.trim();
    let dir = dir.trim().trim_end_matches('/');
    (!host.is_empty() && !dir.is_empty()).then_some((host, dir))
}

fn remote_target(config: &Config) -> Result<Option<RemoteTarget>, String> {
    if let Some(target) = config.backup_ssh_target.as_deref() {
        let (host, dir) =
            split_ssh_target(target).ok_or_else(|| "Invalid backup_ssh_target.".to_owned())?;
        return Ok(Some(RemoteTarget::Ssh {
            host: host.to_owned(),
            dir: dir.to_owned(),
            port: config.backup_ssh_port,
        }));
    }
    if let Some(uri) = config.backup_s3_uri.as_deref() {
        return Ok(Some(RemoteTarget::S3 {
            base: uri.trim_end_matches('/').to_owned(),
        }));
    }
    Ok(None)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn run_command(
    command: &mut Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    command.kill_on_drop(true);
    match time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("Cannot run command: {error}")),
        Err(_) => Err("Remote backup command timed out.".to_owned()),
    }
}

fn ensure_command_success(label: &str, output: &std::process::Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("{label} failed: {}", stderr.trim()))
}

async fn encrypt_backup(
    key: &FsPath,
    input: &FsPath,
    output: &FsPath,
    timeout: Duration,
) -> Result<(), String> {
    let mut command = Command::new("gpg");
    command
        .arg("--batch")
        .arg("--yes")
        .arg("--pinentry-mode")
        .arg("loopback")
        .arg("--passphrase-file")
        .arg(key)
        .arg("--symmetric")
        .arg("--cipher-algo")
        .arg("AES256")
        .arg("--output")
        .arg(output)
        .arg(input);
    let result = run_command(&mut command, timeout).await?;
    ensure_command_success("gpg encryption", &result)
}

async fn upload_file(
    target: &RemoteTarget,
    local: &FsPath,
    remote_name: &str,
    timeout: Duration,
) -> Result<(), String> {
    let expected = sha256_hex(
        &fs::read(local).map_err(|error| format!("Cannot read upload artifact: {error}"))?,
    );
    match target {
        RemoteTarget::Ssh { host, dir, port } => {
            let mut mkdir = Command::new("ssh");
            if *port != 22 {
                mkdir.arg("-p").arg(port.to_string());
            }
            mkdir
                .arg(host)
                .arg(format!("mkdir -p {}", shell_quote(dir)));
            let output = run_command(&mut mkdir, timeout).await?;
            ensure_command_success("ssh mkdir", &output)?;

            let mut command = Command::new("scp");
            command.arg("-q");
            if *port != 22 {
                command.arg("-P").arg(port.to_string());
            }
            command
                .arg(local)
                .arg(format!("{host}:{}/", dir.trim_end_matches('/')));
            let output = run_command(&mut command, timeout).await?;
            ensure_command_success("scp upload", &output)?;

            let remote_path = format!("{}/{}", dir.trim_end_matches('/'), remote_name);
            let mut verify = Command::new("ssh");
            if *port != 22 {
                verify.arg("-p").arg(port.to_string());
            }
            verify
                .arg(host)
                .arg(format!("cat {}", shell_quote(&remote_path)));
            let output = run_command(&mut verify, timeout).await?;
            ensure_command_success("ssh verification", &output)?;
            if sha256_hex(&output.stdout) != expected {
                return Err("Remote checksum verification failed (ssh).".to_owned());
            }
        }
        RemoteTarget::S3 { base } => {
            let uri = format!("{}/{}", base, remote_name);
            let mut command = Command::new("aws");
            command
                .arg("s3")
                .arg("cp")
                .arg(local)
                .arg(&uri)
                .arg("--only-show-errors");
            let output = run_command(&mut command, timeout).await?;
            ensure_command_success("aws s3 upload", &output)?;

            let mut verify = Command::new("aws");
            verify
                .arg("s3")
                .arg("cp")
                .arg(&uri)
                .arg("-")
                .arg("--only-show-errors");
            let output = run_command(&mut verify, timeout).await?;
            ensure_command_success("aws s3 verification", &output)?;
            if sha256_hex(&output.stdout) != expected {
                return Err("Remote checksum verification failed (s3).".to_owned());
            }
        }
    }
    Ok(())
}

async fn upload_backup(
    state: &AppState,
    json_path: &FsPath,
    checksum: &str,
) -> Result<(String, String), String> {
    let target = remote_target(&state.config)?
        .ok_or_else(|| "Remote backup target is not configured.".to_owned())?;
    let timeout = Duration::from_secs(state.config.backup_remote_timeout_seconds);
    let json_name = json_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Invalid backup filename.".to_owned())?;
    let temp_dir = std::env::temp_dir().join(format!(
        "datatask-upload-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = async {
        if let Some(key) = &state.config.backup_encryption_key_file {
            fs::create_dir_all(&temp_dir)
                .map_err(|error| format!("Cannot create upload temp dir: {error}"))?;
            let encrypted_name = format!("{json_name}.enc");
            let encrypted_path = temp_dir.join(&encrypted_name);
            encrypt_backup(key, json_path, &encrypted_path, timeout).await?;
            let encrypted = fs::read(&encrypted_path)
                .map_err(|error| format!("Cannot read encrypted backup: {error}"))?;
            let encrypted_checksum = sha256_hex(&encrypted);
            let sidecar = temp_dir.join(format!("{encrypted_name}.sha256"));
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&sidecar)
                .map_err(|error| format!("Cannot write upload checksum: {error}"))?;
            file.write_all(format!("{encrypted_checksum}  {encrypted_name}\n").as_bytes())
                .map_err(|error| format!("Cannot write upload checksum: {error}"))?;
            upload_file(&target, &encrypted_path, &encrypted_name, timeout).await?;
            upload_file(
                &target,
                &sidecar,
                &format!("{encrypted_name}.sha256"),
                timeout,
            )
            .await?;
            Ok((encrypted_name, encrypted_checksum))
        } else {
            let sidecar = checksum_path(json_path);
            upload_file(&target, json_path, json_name, timeout).await?;
            upload_file(&target, &sidecar, &format!("{json_name}.sha256"), timeout).await?;
            Ok((json_name.to_owned(), checksum.to_owned()))
        }
    }
    .await;
    let _ = fs::remove_dir_all(&temp_dir);
    result
}

fn is_remote_artifact(name: &str) -> bool {
    name.starts_with("datatask-jobs-") && (name.ends_with(".json") || name.ends_with(".json.enc"))
}

fn remote_artifacts_to_prune(mut names: Vec<String>, retention: u32) -> Vec<String> {
    if retention == 0 {
        return Vec::new();
    }
    names.retain(|name| is_remote_artifact(name));
    names.sort();
    let remove = names.len().saturating_sub(retention as usize);
    names.into_iter().take(remove).collect()
}

async fn list_remote_artifacts(
    target: &RemoteTarget,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    match target {
        RemoteTarget::Ssh { host, dir, port } => {
            let mut command = Command::new("ssh");
            if *port != 22 {
                command.arg("-p").arg(port.to_string());
            }
            command.arg(host).arg(format!("ls -1 {}", shell_quote(dir)));
            let output = run_command(&mut command, timeout).await?;
            ensure_command_success("ssh list backups", &output)?;
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|name| is_remote_artifact(name))
                .map(str::to_owned)
                .collect())
        }
        RemoteTarget::S3 { base } => {
            let mut command = Command::new("aws");
            command.arg("s3").arg("ls").arg(format!("{base}/"));
            let output = run_command(&mut command, timeout).await?;
            ensure_command_success("aws s3 list backups", &output)?;
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.split_whitespace().last())
                .filter(|name| is_remote_artifact(name))
                .map(str::to_owned)
                .collect())
        }
    }
}

async fn delete_remote_artifacts(
    target: &RemoteTarget,
    names: &[String],
    timeout: Duration,
) -> Result<(), String> {
    match target {
        RemoteTarget::Ssh { host, dir, port } => {
            let mut script = format!("cd {} && rm -f --", shell_quote(dir));
            for name in names {
                script.push(' ');
                script.push_str(&shell_quote(name));
                script.push(' ');
                script.push_str(&shell_quote(&format!("{name}.sha256")));
            }
            let mut command = Command::new("ssh");
            if *port != 22 {
                command.arg("-p").arg(port.to_string());
            }
            command.arg(host).arg(script);
            let output = run_command(&mut command, timeout).await?;
            ensure_command_success("ssh prune backups", &output)
        }
        RemoteTarget::S3 { base } => {
            for name in names {
                for key in [name.clone(), format!("{name}.sha256")] {
                    let mut command = Command::new("aws");
                    command
                        .arg("s3")
                        .arg("rm")
                        .arg(format!("{base}/{key}"))
                        .arg("--only-show-errors");
                    let output = run_command(&mut command, timeout).await?;
                    ensure_command_success("aws s3 prune backup", &output)?;
                }
            }
            Ok(())
        }
    }
}

async fn prune_remote_backups(
    target: &RemoteTarget,
    retention: u32,
    timeout: Duration,
) -> Result<usize, String> {
    if retention == 0 {
        return Ok(0);
    }
    let names = list_remote_artifacts(target, timeout).await?;
    let remove = remote_artifacts_to_prune(names, retention);
    if remove.is_empty() {
        return Ok(0);
    }
    delete_remote_artifacts(target, &remove, timeout).await?;
    Ok(remove.len())
}

async fn read_remote_file(
    target: &RemoteTarget,
    name: &str,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    match target {
        RemoteTarget::Ssh { host, dir, port } => {
            let path = format!("{}/{}", dir.trim_end_matches('/'), name);
            let mut command = Command::new("ssh");
            if *port != 22 {
                command.arg("-p").arg(port.to_string());
            }
            command.arg(host).arg(format!("cat {}", shell_quote(&path)));
            let output = run_command(&mut command, timeout).await?;
            ensure_command_success("ssh read backup", &output)?;
            Ok(output.stdout)
        }
        RemoteTarget::S3 { base } => {
            let mut command = Command::new("aws");
            command
                .arg("s3")
                .arg("cp")
                .arg(format!("{base}/{name}"))
                .arg("-")
                .arg("--only-show-errors");
            let output = run_command(&mut command, timeout).await?;
            ensure_command_success("aws s3 read backup", &output)?;
            Ok(output.stdout)
        }
    }
}

async fn verify_remote_backups(
    target: &RemoteTarget,
    timeout: Duration,
) -> Result<BackupVerifySummary, String> {
    let names = list_remote_artifacts(target, timeout).await?;
    let mut items = Vec::with_capacity(names.len());
    for name in names {
        let sidecar_name = format!("{name}.sha256");
        let result = async {
            let sidecar = read_remote_file(target, &sidecar_name, timeout).await?;
            let expected = String::from_utf8_lossy(&sidecar)
                .split_whitespace()
                .next()
                .ok_or_else(|| "empty checksum sidecar".to_owned())?
                .to_owned();
            let data = read_remote_file(target, &name, timeout).await?;
            let actual = sha256_hex(&data);
            if !expected.eq_ignore_ascii_case(&actual) {
                return Err("checksum mismatch".to_owned());
            }
            Ok(())
        }
        .await;
        items.push(BackupVerifyItem {
            name,
            ok: result.is_ok(),
            error: result.err(),
        });
    }
    let ok = items.iter().filter(|item| item.ok).count();
    Ok(BackupVerifySummary {
        timestamp: Local::now().to_rfc3339(),
        checked: items.len(),
        ok,
        failed: items.len() - ok,
        items,
    })
}

fn backup_alert_key(summary: &BackupVerifySummary) -> Option<String> {
    if summary.checked == 0 {
        return Some("missing".to_owned());
    }
    if summary.failed > 0 {
        let mut names: Vec<&str> = summary
            .items
            .iter()
            .filter(|item| !item.ok)
            .map(|item| item.name.as_str())
            .collect();
        names.sort();
        return Some(format!("failed:{}", names.join(",")));
    }
    None
}

async fn send_backup_alert(state: &AppState, target: &RemoteTarget, summary: &BackupVerifySummary) {
    let Some(url) = state.config.backup_alert_url.as_deref() else {
        return;
    };
    let event = if summary.checked == 0 {
        "backup.missing"
    } else {
        "backup.verify_failed"
    };
    let payload = json!({
        "event": event,
        "timestamp": summary.timestamp,
        "remote": target.label(),
        "checked": summary.checked,
        "ok": summary.ok,
        "failed": summary.failed,
        "items": summary.items,
    });
    match state.http_client.post(url).json(&payload).send().await {
        Ok(response) => {
            let status = response.status();
            state.record_backup_event(
                BackupEventKind::Alert,
                status.is_success(),
                Some(format!("HTTP {}", status.as_u16())),
            );
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "backup.alert",
                "status": status.as_u16(),
                "alert": event,
            }));
        }
        Err(error) => {
            state.record_backup_event(BackupEventKind::Alert, false, Some(error.to_string()));
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "backup.alert",
                "error": error.to_string(),
                "alert": event,
            }));
        }
    }
}

async fn record_backup_verify(
    state: &AppState,
    target: &RemoteTarget,
    summary: &BackupVerifySummary,
) {
    *state
        .last_backup_verify
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(summary.clone());
    state.audit(json!({
        "ts": Local::now().to_rfc3339(),
        "event": "backup.verify",
        "remote": target.label(),
        "checked": summary.checked,
        "ok": summary.ok,
        "failed": summary.failed,
    }));
    if summary.failed > 0 {
        state.audit(json!({
            "ts": Local::now().to_rfc3339(),
            "event": "backup.verify_error",
            "remote": target.label(),
            "items": summary.items,
        }));
    }
    state.verify_history.append(summary);

    let key = backup_alert_key(summary);
    let should_send = {
        let mut last = state
            .last_backup_alert
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.as_ref() == key.as_ref() {
            false
        } else {
            *last = key.clone();
            true
        }
    };
    if should_send {
        send_backup_alert(state, target, summary).await;
    }
}

async fn run_remote_verify(state: Arc<AppState>) {
    let target = match remote_target(&state.config) {
        Ok(Some(target)) => target,
        Ok(None) => return,
        Err(error) => {
            eprintln!("Cannot configure remote backup verification: {error}");
            return;
        }
    };
    let timeout = Duration::from_secs(state.config.backup_remote_timeout_seconds);
    match verify_remote_backups(&target, timeout).await {
        Ok(summary) => record_backup_verify(&state, &target, &summary).await,
        Err(error) => {
            eprintln!("Remote backup verification failed: {error}");
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "backup.verify_error",
                "remote": target.label(),
                "error": error,
            }));
        }
    }
}

async fn verify_backups(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let target = remote_target(&state.config)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Remote backup is not configured."))?;
    let timeout = Duration::from_secs(state.config.backup_remote_timeout_seconds);
    let summary = verify_remote_backups(&target, timeout)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    record_backup_verify(&state, &target, &summary).await;
    Ok(Json(json!(summary)))
}

async fn run_backup(state: Arc<AppState>) {
    let Some(dir) = &state.config.backup_dir else {
        return;
    };
    let jobs = match list_jobs_cached(&state.config, &state.job_cache) {
        Ok(jobs) => jobs,
        Err(error) => {
            eprintln!("Cannot collect jobs for backup: {}", error.1);
            return;
        }
    };
    match write_backup(dir, state.config.backup_retention, &jobs) {
        Ok((path, checksum)) => {
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "backup.created",
                "path": path.display().to_string(),
                "jobs": jobs.len(),
                "sha256": checksum,
            }));
            match remote_target(&state.config) {
                Ok(Some(target)) => {
                    let timeout = Duration::from_secs(state.config.backup_remote_timeout_seconds);
                    match upload_backup(&state, &path, &checksum).await {
                        Ok((file, remote_checksum)) => {
                            state.record_backup_event(
                                BackupEventKind::Upload,
                                true,
                                Some(file.clone()),
                            );
                            state.audit(json!({
                                "ts": Local::now().to_rfc3339(),
                                "event": "backup.uploaded",
                                "remote": target.label(),
                                "file": file,
                                "sha256": remote_checksum,
                            }));
                            match prune_remote_backups(
                                &target,
                                state.config.backup_retention,
                                timeout,
                            )
                            .await
                            {
                                Ok(deleted) => {
                                    state.record_backup_event(
                                        BackupEventKind::Prune,
                                        true,
                                        Some(format!("{deleted} removidos")),
                                    );
                                    if deleted > 0 {
                                        state.audit(json!({
                                            "ts": Local::now().to_rfc3339(),
                                            "event": "backup.pruned",
                                            "remote": target.label(),
                                            "deleted": deleted,
                                        }));
                                    }
                                }
                                Err(error) => {
                                    state.record_backup_event(
                                        BackupEventKind::Prune,
                                        false,
                                        Some(error.clone()),
                                    );
                                    eprintln!("Remote backup prune failed: {error}");
                                    state.audit(json!({
                                        "ts": Local::now().to_rfc3339(),
                                        "event": "backup.prune_error",
                                        "remote": target.label(),
                                        "error": error,
                                    }));
                                }
                            }
                        }
                        Err(error) => {
                            state.record_backup_event(
                                BackupEventKind::Upload,
                                false,
                                Some(error.clone()),
                            );
                            eprintln!("Remote backup upload failed: {error}");
                            state.audit(json!({
                                "ts": Local::now().to_rfc3339(),
                                "event": "backup.upload_error",
                                "remote": target.label(),
                                "error": error,
                            }));
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!("Cannot configure remote backup: {error}"),
            }
        }
        Err(error) => {
            eprintln!("Backup failed: {error}");
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "backup.error",
                "error": error,
            }));
        }
    }
}

#[derive(Clone, Serialize)]
struct BackupInfo {
    name: String,
    size: u64,
    modified: Option<String>,
    sha256: Option<String>,
    checksum_ok: bool,
    valid_json: bool,
    jobs: usize,
}

#[derive(Clone, Copy)]
enum BackupEventKind {
    Upload,
    Prune,
    Alert,
}

#[derive(Clone, Serialize)]
struct BackupEvent {
    timestamp: String,
    ok: bool,
    detail: Option<String>,
}

#[derive(Clone, Default, Serialize)]
struct BackupHealth {
    last_upload: Option<BackupEvent>,
    last_prune: Option<BackupEvent>,
    last_alert: Option<BackupEvent>,
}

#[derive(Clone, Serialize, Deserialize)]
struct BackupVerifyItem {
    name: String,
    ok: bool,
    error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct BackupVerifySummary {
    timestamp: String,
    checked: usize,
    ok: usize,
    failed: usize,
    items: Vec<BackupVerifyItem>,
}

fn checksum_path(path: &FsPath) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".sha256");
    PathBuf::from(value)
}

fn expected_checksum(path: &FsPath) -> Option<String> {
    let text = fs::read_to_string(checksum_path(path)).ok()?;
    text.split_whitespace().next().map(str::to_owned)
}

fn valid_backup_name(name: &str) -> bool {
    name.len() <= 128
        && name.starts_with("datatask-jobs-")
        && name.ends_with(".json")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn backup_info(dir: &FsPath, name: &str) -> Result<BackupInfo, String> {
    let path = dir.join(name);
    let metadata =
        fs::metadata(&path).map_err(|error| format!("Cannot inspect backup: {error}"))?;
    let data = fs::read(&path).map_err(|error| format!("Cannot read backup: {error}"))?;
    let actual = sha256_hex(&data);
    let expected = expected_checksum(&path);
    let checksum_ok = expected
        .as_deref()
        .is_some_and(|checksum| checksum.eq_ignore_ascii_case(&actual));
    let parsed = serde_json::from_slice::<Value>(&data);
    let (valid_json, jobs) = match &parsed {
        Ok(document) => (
            true,
            document
                .get("jobs")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        ),
        Err(_) => (false, 0),
    };
    let modified = metadata
        .modified()
        .ok()
        .map(|time| DateTime::<Local>::from(time).to_rfc3339());
    Ok(BackupInfo {
        name: name.to_owned(),
        size: metadata.len(),
        modified,
        sha256: expected,
        checksum_ok,
        valid_json,
        jobs,
    })
}

fn list_backup_infos(config: &Config) -> Result<Vec<BackupInfo>, String> {
    let Some(dir) = &config.backup_dir else {
        return Ok(Vec::new());
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Cannot list backups: {error}")),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| valid_backup_name(name))
        .collect();
    names.sort_by(|left, right| right.cmp(left));
    names.truncate(100);
    let mut items = Vec::new();
    for name in names {
        match backup_info(dir, &name) {
            Ok(info) => items.push(info),
            Err(error) => eprintln!("Cannot inspect backup '{name}': {error}"),
        }
    }
    Ok(items)
}

#[derive(Deserialize)]
struct VerifyHistoryQuery {
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn get_backup_verifications(
    State(state): State<Arc<AppState>>,
    Query(query): Query<VerifyHistoryQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let mut entries = state.verify_history.read();
    entries.reverse();
    let total = entries.len();
    let items: Vec<BackupVerifySummary> = entries.into_iter().skip(offset).take(limit).collect();
    Ok(Json(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

async fn get_backup_health(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let enabled = state.config.backup_dir.is_some();
    let local = list_backup_infos(&state.config)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let latest_local = local.first().cloned();
    let last_verify = state
        .last_backup_verify
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let health = state
        .backup_health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let remote = if state.config.backup_ssh_target.is_some() {
        "ssh"
    } else if state.config.backup_s3_uri.is_some() {
        "s3"
    } else {
        ""
    };
    let mut status = if enabled { "ok" } else { "disabled" };
    let mut problems: Vec<String> = Vec::new();
    if !enabled {
        problems.push("backup_dir não configurado".to_owned());
    } else {
        if local.is_empty() {
            status = "warning";
            problems.push("nenhum backup local encontrado".to_owned());
        }
        if remote.is_empty() {
            if status == "ok" {
                status = "warning";
            }
            problems.push("destino remoto não configurado".to_owned());
        }
        match &last_verify {
            Some(summary) if summary.failed > 0 => {
                status = "error";
                problems.push(format!(
                    "{} falha(s) na última verificação remota",
                    summary.failed
                ));
            }
            Some(summary) if summary.checked == 0 => {
                if status != "error" {
                    status = "warning";
                }
                problems.push("nenhum backup remoto encontrado".to_owned());
            }
            None if !remote.is_empty() => {
                if status != "error" {
                    status = "warning";
                }
                problems.push("nenhuma verificação remota executada".to_owned());
            }
            _ => {}
        }
        if health.last_upload.as_ref().is_some_and(|event| !event.ok) {
            status = "error";
            problems.push("o último upload remoto falhou".to_owned());
        }
        if health.last_prune.as_ref().is_some_and(|event| !event.ok) {
            if status != "error" {
                status = "warning";
            }
            problems.push("a última limpeza remota falhou".to_owned());
        }
        if health.last_alert.as_ref().is_some_and(|event| !event.ok) {
            if status != "error" {
                status = "warning";
            }
            problems.push("o último alerta não foi entregue".to_owned());
        }
    }

    Ok(Json(json!({
        "status": status,
        "enabled": enabled,
        "remote": remote,
        "encryption": state.config.backup_encryption_key_file.is_some(),
        "alerts_enabled": state.config.backup_alert_url.is_some(),
        "retention": state.config.backup_retention,
        "verify_interval_seconds": state.config.backup_verify_interval_seconds,
        "local_count": local.len(),
        "latest_local": latest_local,
        "last_upload": health.last_upload,
        "last_prune": health.last_prune,
        "last_alert": health.last_alert,
        "last_verify": last_verify,
        "problems": problems,
        "now": Local::now().to_rfc3339(),
    })))
}

async fn get_backups(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let items = list_backup_infos(&state.config)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(json!({
        "enabled": state.config.backup_dir.is_some(),
        "dir": state
            .config
            .backup_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        "items": items,
    })))
}

async fn get_backup_file(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    if !valid_backup_name(&name) {
        return Err(api_error(StatusCode::BAD_REQUEST, "Invalid backup name."));
    }
    let Some(dir) = &state.config.backup_dir else {
        return Err(api_error(StatusCode::NOT_FOUND, "Backups are disabled."));
    };
    let path = dir.join(&name);
    let data = fs::read(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => api_error(StatusCode::NOT_FOUND, "Backup not found."),
        _ => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot read backup: {error}"),
        ),
    })?;
    let actual = sha256_hex(&data);
    let Some(expected) = expected_checksum(&path) else {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Backup checksum file is missing.",
        ));
    };
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Backup checksum does not match.",
        ));
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "Invalid filename."))?,
    );
    if let Ok(value) = HeaderValue::from_str(&actual) {
        headers.insert(HeaderName::from_static("x-checksum-sha256"), value);
    }
    Ok((headers, data).into_response())
}

fn client_ip(state: &AppState, request: &Request<Body>) -> IpAddr {
    let peer = peer_ip(request);
    if state.config.trust_proxy_headers
        && is_trusted_proxy(peer, &state.config.trusted_proxy_ips)
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
    peer
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpsDecision {
    Allow,
    UntrustedProxy,
    UpgradeRequired,
}

fn https_decision(
    tls_proxy: bool,
    path: &str,
    peer_is_loopback: bool,
    peer_is_trusted: bool,
    forwarded_https: bool,
) -> HttpsDecision {
    if !tls_proxy || matches!(path, "/health" | "/readyz") || peer_is_loopback {
        return HttpsDecision::Allow;
    }
    if !peer_is_trusted {
        return HttpsDecision::UntrustedProxy;
    }
    if forwarded_https {
        HttpsDecision::Allow
    } else {
        HttpsDecision::UpgradeRequired
    }
}

async fn https_guard(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !state.config.tls_terminated_by_proxy {
        return next.run(request).await;
    }
    let path = request.uri().path().to_owned();
    let peer = peer_ip(&request);
    let trusted = is_trusted_proxy(peer, &state.config.trusted_proxy_ips);
    let forwarded_https = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("https"));
    match https_decision(
        state.config.tls_terminated_by_proxy,
        &path,
        peer.is_loopback(),
        trusted,
        forwarded_https,
    ) {
        HttpsDecision::Allow => next.run(request).await,
        HttpsDecision::UntrustedProxy => {
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "request.rejected",
                "reason": "untrusted_proxy",
                "method": request.method().as_str(),
                "path": path,
                "ip": peer.to_string(),
            }));
            (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "Requests must come through a trusted TLS proxy."
                })),
            )
                .into_response()
        }
        HttpsDecision::UpgradeRequired => {
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "request.rejected",
                "reason": "https_required",
                "method": request.method().as_str(),
                "path": path,
                "ip": peer.to_string(),
            }));
            (
                StatusCode::UPGRADE_REQUIRED,
                Json(json!({
                    "error": "HTTPS required. Access this service through the configured TLS proxy."
                })),
            )
                .into_response()
        }
    }
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
    /// Incremented on user-configuration changes; used for optimistic locking.
    #[serde(default = "default_job_version")]
    version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    retries: u32,
    #[serde(default = "default_retry_delay_seconds")]
    retry_delay_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notify_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PortableJob {
    name: String,
    #[serde(default)]
    description: String,
    datetime: String,
    action: String,
    #[serde(default = "default_active_status")]
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    retries: u32,
    #[serde(default = "default_retry_delay_seconds")]
    retry_delay_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notify_url: Option<String>,
}

impl From<&Job> for PortableJob {
    fn from(job: &Job) -> Self {
        Self {
            name: job.name.clone(),
            description: job.description.clone(),
            datetime: job.datetime.clone(),
            action: job.action.clone(),
            status: job.status.clone(),
            working_dir: job.working_dir.clone(),
            timeout_seconds: job.timeout_seconds,
            retries: job.retries,
            retry_delay_seconds: job.retry_delay_seconds,
            notify_url: job.notify_url.clone(),
        }
    }
}

#[derive(Deserialize)]
struct JobsImport {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    jobs: Vec<PortableJob>,
    #[serde(default)]
    on_conflict: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

fn default_active_status() -> String {
    "ativo".to_owned()
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

#[derive(Clone, PartialEq, Eq)]
struct JobStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
}

struct JobCache {
    state: StdMutex<JobCacheState>,
}

#[derive(Default)]
struct JobCacheState {
    stamp: Vec<JobStamp>,
    jobs: Vec<Job>,
}

impl JobCache {
    fn new() -> Self {
        Self {
            state: StdMutex::new(JobCacheState::default()),
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
    retries: Option<u32>,
    retry_delay_seconds: Option<u64>,
    notify_url: Option<String>,
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
    retries: Option<u32>,
    retry_delay_seconds: Option<u64>,
    notify_url: Option<String>,
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

fn job_etag(version: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{version}\"")).unwrap_or(HeaderValue::from_static("\"1\""))
}

fn job_response(status: StatusCode, job: Job) -> Response {
    let etag = job_etag(job.version);
    let mut response = (status, Json(job)).into_response();
    response.headers_mut().insert(header::ETAG, etag);
    response
}

fn job_conflict_response(job: Job) -> Response {
    let etag = job_etag(job.version);
    let mut response = (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "Job was modified by another client. Reload it before saving.",
            "current": job,
        })),
    )
        .into_response();
    response.headers_mut().insert(header::ETAG, etag);
    response
}

fn if_match_allows(headers: &HeaderMap, version: u64) -> ApiResult<bool> {
    let Some(value) = headers.get(header::IF_MATCH) else {
        return Ok(true);
    };
    let text = value
        .to_str()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Invalid If-Match header."))?;
    let text = text.trim();
    if text == "*" {
        return Ok(true);
    }
    let mut saw_numeric_tag = false;
    for part in text.split(',') {
        let mut tag = part.trim();
        if let Some(rest) = tag.strip_prefix("W/") {
            tag = rest.trim();
        }
        let tag = tag
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(tag);
        if let Ok(candidate) = tag.parse::<u64>() {
            saw_numeric_tag = true;
            if candidate == version {
                return Ok(true);
            }
        }
    }
    if !saw_numeric_tag {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "If-Match must contain a numeric job version.",
        ));
    }
    Ok(false)
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
    config.history_file = match config.history_file.take() {
        None => Some(config.logs_dir.join("history.jsonl")),
        Some(path) if path.as_os_str().is_empty() => None,
        Some(path) if path.is_relative() => Some(base.join(path)),
        Some(path) => Some(path),
    };
    config.backup_dir = match config.backup_dir.take() {
        None => None,
        Some(path) if path.as_os_str().is_empty() => None,
        Some(path) if path.is_relative() => Some(base.join(path)),
        Some(path) => Some(path),
    };
    config.backup_encryption_key_file = match config.backup_encryption_key_file.take() {
        None => None,
        Some(path) if path.as_os_str().is_empty() => None,
        Some(path) if path.is_relative() => Some(base.join(path)),
        Some(path) => Some(path),
    };
    config.backup_ssh_target = config
        .backup_ssh_target
        .take()
        .filter(|target| !target.trim().is_empty());
    config.backup_s3_uri = config
        .backup_s3_uri
        .take()
        .filter(|uri| !uri.trim().is_empty());
    config.backup_alert_url = config
        .backup_alert_url
        .take()
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty());
    if let Some(url) = &config.backup_alert_url {
        normalize_notify_url(Some(url)).map_err(|error| error.1)?;
    }
    for path in [&mut config.tls_cert_file, &mut config.tls_key_file] {
        if let Some(path) = path
            && path.is_relative()
        {
            *path = base.join(&*path);
        }
    }
    config.tls_cert_file = config
        .tls_cert_file
        .take()
        .filter(|path| !path.as_os_str().is_empty());
    config.tls_key_file = config
        .tls_key_file
        .take()
        .filter(|path| !path.as_os_str().is_empty());
    let token = resolve_api_token(&mut config, base)?;

    if config.poll_interval_seconds == 0 {
        return Err("poll_interval_seconds must be greater than zero.".to_owned());
    }
    if config.command_timeout_seconds == 0 {
        return Err("command_timeout_seconds must be greater than zero.".to_owned());
    }
    if config.notification_timeout_seconds == 0 {
        return Err("notification_timeout_seconds must be greater than zero.".to_owned());
    }
    if config.max_concurrent_jobs == 0 {
        return Err("max_concurrent_jobs must be greater than zero.".to_owned());
    }
    if let Some(backup_dir) = &config.backup_dir {
        if config.backup_interval_seconds == 0 {
            return Err("backup_interval_seconds must be greater than zero.".to_owned());
        }
        if config.backup_retention > 365 {
            return Err("backup_retention must be between 0 and 365.".to_owned());
        }
        if backup_dir == &config.jobs_dir {
            return Err("backup_dir must be different from jobs_dir.".to_owned());
        }
    }
    let remote_ssh = config.backup_ssh_target.is_some();
    let remote_s3 = config.backup_s3_uri.is_some();
    if remote_ssh && remote_s3 {
        return Err("Set either backup_ssh_target or backup_s3_uri, not both.".to_owned());
    }
    if (remote_ssh || remote_s3) && config.backup_dir.is_none() {
        return Err("Remote backup requires backup_dir to be configured.".to_owned());
    }
    if config.backup_encryption_key_file.is_some() && !remote_ssh && !remote_s3 {
        return Err("backup_encryption_key_file requires a remote backup destination.".to_owned());
    }
    if config.backup_alert_url.is_some() && !remote_ssh && !remote_s3 {
        return Err("backup_alert_url requires a remote backup destination.".to_owned());
    }
    if remote_ssh {
        if config.backup_ssh_port == 0 {
            return Err("backup_ssh_port must be greater than zero.".to_owned());
        }
        if split_ssh_target(config.backup_ssh_target.as_deref().unwrap_or("")).is_none() {
            return Err("backup_ssh_target must use the form user@host:/absolute/dir.".to_owned());
        }
    }
    if let Some(uri) = &config.backup_s3_uri
        && (!uri.starts_with("s3://") || uri.len() <= 5)
    {
        return Err("backup_s3_uri must start with s3://.".to_owned());
    }
    if config.backup_remote_timeout_seconds == 0 {
        return Err("backup_remote_timeout_seconds must be greater than zero.".to_owned());
    }
    if let Some(key) = &config.backup_encryption_key_file {
        let metadata = fs::metadata(key).map_err(|error| {
            format!(
                "Cannot read backup encryption key {}: {error}",
                key.display()
            )
        })?;
        if !has_private_permissions(&metadata) {
            return Err(format!(
                "Backup encryption key {} must not be accessible by group or others (chmod 600).",
                key.display()
            ));
        }
    }
    if config.auth_failure_limit > 0 && config.auth_failure_window_seconds == 0 {
        return Err("auth_failure_window_seconds must be greater than zero.".to_owned());
    }
    if config.auth_failure_limit > 0 && config.auth_lockout_seconds == 0 {
        return Err("auth_lockout_seconds must be greater than zero.".to_owned());
    }
    let has_native_tls = native_tls(&config);
    match (&config.tls_cert_file, &config.tls_key_file) {
        (Some(_), Some(_)) => {
            if config.tls_terminated_by_proxy {
                return Err(
                    "Choose native TLS (tls_cert_file/tls_key_file) or tls_terminated_by_proxy, not both."
                        .to_owned(),
                );
            }
            if let Some(key) = &config.tls_key_file
                && let Ok(metadata) = fs::metadata(key)
                && !has_private_permissions(&metadata)
            {
                return Err(format!(
                    "TLS key {} must not be accessible by group or others (chmod 600).",
                    key.display()
                ));
            }
        }
        (None, None) => {}
        _ => {
            return Err(
                "Set both tls_cert_file and tls_key_file for native TLS, or neither.".to_owned(),
            );
        }
    }
    if !has_native_tls && !bind_is_loopback(&config.bind_address) && !config.tls_terminated_by_proxy
    {
        return Err(format!(
            "bind_address '{}' would expose plaintext HTTP. Keep bind_address = \"127.0.0.1\" behind a TLS reverse proxy, set tls_terminated_by_proxy = true only when the port is reachable exclusively by that proxy, or configure native TLS with tls_cert_file/tls_key_file.",
            config.bind_address
        ));
    }
    if !has_native_tls
        && config.tls_terminated_by_proxy
        && !bind_is_loopback(&config.bind_address)
        && config.trusted_proxy_ips.is_empty()
    {
        return Err(
            "tls_terminated_by_proxy with a non-loopback bind requires trusted_proxy_ips with the proxy address(es) (for example [\"10.0.0.10\"])."
                .to_owned(),
        );
    }
    if !config.tls_terminated_by_proxy
        && !config.trust_proxy_headers
        && !config.trusted_proxy_ips.is_empty()
    {
        eprintln!(
            "warning: trusted_proxy_ips is ignored because tls_terminated_by_proxy and trust_proxy_headers are false."
        );
    }
    if config.tls_terminated_by_proxy
        && !bind_is_loopback(&config.bind_address)
        && !config.trust_proxy_headers
    {
        eprintln!(
            "warning: tls_terminated_by_proxy is set but trust_proxy_headers is false; \
             rate limiting will see the proxy IP instead of each client."
        );
    }
    if config.tls_terminated_by_proxy && !bind_is_loopback(&config.bind_address) {
        eprintln!(
            "warning: bind_address '{}' must be firewalled so only the TLS proxy can reach this port.",
            config.bind_address
        );
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
const MAX_RETRIES: u32 = 10;
const MAX_RETRY_DELAY_SECONDS: u64 = 3_600;

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

fn normalize_retries(value: u32) -> ApiResult<u32> {
    if value > MAX_RETRIES {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("retries must be between 0 and {MAX_RETRIES}."),
        ));
    }
    Ok(value)
}

fn normalize_retry_delay(value: u64) -> ApiResult<u64> {
    let value = if value == 0 {
        default_retry_delay_seconds()
    } else {
        value
    };
    if value > MAX_RETRY_DELAY_SECONDS {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("retry_delay_seconds must be between 1 and {MAX_RETRY_DELAY_SECONDS}."),
        ));
    }
    Ok(value)
}

fn normalize_notify_url(value: Option<&str>) -> ApiResult<Option<String>> {
    let Some(trimmed) = value.map(str::trim).filter(|url| !url.is_empty()) else {
        return Ok(None);
    };
    if trimmed.len() > 2048 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "notify_url must be at most 2048 bytes.",
        ));
    }
    let url = reqwest::Url::parse(trimmed)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "notify_url must be a valid URL."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "notify_url must use http or https.",
        ));
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "notify_url must include a host and must not embed credentials.",
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

fn retry_delay(attempt: u32, base_seconds: u64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(10);
    let multiplier = 1u64 << exponent;
    Duration::from_secs(
        base_seconds
            .max(1)
            .saturating_mul(multiplier)
            .min(MAX_RETRY_DELAY_SECONDS),
    )
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
    job.retries = job.retries.min(MAX_RETRIES);
    job.retry_delay_seconds = job.retry_delay_seconds.clamp(1, MAX_RETRY_DELAY_SECONDS);
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

fn job_stamp(config: &Config) -> ApiResult<Vec<JobStamp>> {
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
    let mut stamps = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot read jobs directory entry: {error}"),
            )
        })?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Cannot inspect job '{}': {error}", path.display()),
                ));
            }
        };
        stamps.push(JobStamp {
            path,
            modified: metadata.modified().ok(),
            len: metadata.len(),
        });
    }
    stamps.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(stamps)
}

/// Returns jobs from memory when the directory metadata is unchanged, otherwise
/// re-reads the JSON files. A single corrupt file is skipped.
fn list_jobs_cached(config: &Config, cache: &JobCache) -> ApiResult<Vec<Job>> {
    let stamp = job_stamp(config)?;
    let mut state = cache
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.stamp == stamp {
        return Ok(state.jobs.clone());
    }

    let mut jobs = Vec::with_capacity(stamp.len());
    for file in &stamp {
        let Some(name) = file.path.file_stem().and_then(|stem| stem.to_str()) else {
            eprintln!("Skipping job with an invalid filename.");
            continue;
        };
        match read_job(config, name) {
            Ok(job) => jobs.push(job),
            Err(error) => eprintln!("Skipping invalid job '{name}': {}", error.1),
        }
    }
    jobs.sort_by(|left, right| left.name.cmp(&right.name));
    state.stamp = stamp;
    state.jobs = jobs.clone();
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

fn has_valid_token(state: &AppState, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| {
            // Comparing digests avoids leaking token length and keeps the check
            // constant-time even for differently sized inputs.
            let digest = Sha256::digest(token.as_bytes());
            bool::from(digest.as_slice().ct_eq(&state.token_digest))
        })
}

async fn authenticate(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&state, &request);
    if !has_valid_token(&state, request.headers()) {
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

async fn security_headers(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
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
    if state.config.tls_terminated_by_proxy || native_tls(&state.config) {
        insert_header(headers, "strict-transport-security", "max-age=31536000");
    }
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

async fn metrics(State(state): State<Arc<AppState>>, request: Request<Body>) -> Response {
    let peer = peer_ip(&request);
    if !peer.is_loopback() {
        let ip = client_ip(&state, &request);
        if let Err(rejection) = state.guard.check(ip, Instant::now()) {
            let retry = rejection.retry_after();
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "request.rejected",
                "reason": rejection.reason(),
                "method": request.method().as_str(),
                "path": request.uri().path(),
                "ip": ip.to_string(),
                "retry_after_seconds": retry_seconds(retry),
            }));
            return too_many_requests(retry);
        }
        if !has_valid_token(&state, request.headers()) {
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
    }

    let jobs = match list_jobs_cached(&state.config, &state.job_cache) {
        Ok(jobs) => jobs,
        Err(error) => {
            eprintln!("Cannot collect job metrics: {}", error.1);
            Vec::new()
        }
    };
    let active = jobs.iter().filter(|job| job.status == "ativo").count();
    let running = lock_running(&state).len();
    let last_tick = *state
        .last_tick
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (ready, lag) = scheduler_readiness(
        last_tick,
        Duration::from_secs(state.config.poll_interval_seconds),
        Instant::now(),
    );
    let body = state
        .metrics
        .render(jobs.len(), active, running, ready, lag);
    (
        [
            (
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn dashboard() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(include_str!("../static/index.html")),
    )
}

fn portable_job_to_job(portable: &PortableJob) -> Result<Job, String> {
    if !valid_job_name(&portable.name) {
        return Err("Job name must contain only letters, numbers, '-' or '_'.".to_owned());
    }
    validate_schedule(&portable.datetime).map_err(|error| error.1)?;
    validate_text_length("description", &portable.description, MAX_DESCRIPTION_BYTES)
        .map_err(|error| error.1)?;
    validate_text_length("datetime", &portable.datetime, MAX_DATETIME_BYTES)
        .map_err(|error| error.1)?;
    validate_text_length("action", &portable.action, MAX_ACTION_BYTES).map_err(|error| error.1)?;
    if portable.action.trim().is_empty() {
        return Err("action cannot be empty.".to_owned());
    }
    let status = normalize_status(&portable.status).map_err(|error| error.1)?;
    let working_dir =
        normalize_working_dir(portable.working_dir.as_deref()).map_err(|error| error.1)?;
    let timeout_seconds =
        normalize_timeout(portable.timeout_seconds.unwrap_or(0)).map_err(|error| error.1)?;
    let retries = normalize_retries(portable.retries).map_err(|error| error.1)?;
    let retry_delay_seconds =
        normalize_retry_delay(portable.retry_delay_seconds).map_err(|error| error.1)?;
    let notify_url =
        normalize_notify_url(portable.notify_url.as_deref()).map_err(|error| error.1)?;
    Ok(Job {
        name: portable.name.clone(),
        description: portable.description.clone(),
        datetime: portable.datetime.trim().to_owned(),
        action: portable.action.clone(),
        log: format!("logs/{}.log", portable.name),
        status,
        version: 1,
        last_run: None,
        working_dir,
        timeout_seconds,
        retries,
        retry_delay_seconds,
        notify_url,
        last_result: None,
        last_exit_code: None,
        last_duration_ms: None,
    })
}

async fn export_jobs(State(state): State<Arc<AppState>>) -> ApiResult<Response> {
    let jobs = list_jobs_cached(&state.config, &state.job_cache)?;
    let portable: Vec<PortableJob> = jobs.iter().map(PortableJob::from).collect();
    let document = json!({
        "format": "datatask.jobs",
        "version": 1,
        "exported_at": Local::now().to_rfc3339(),
        "jobs": portable,
    });
    let filename = format!(
        "datatask-jobs-{}.json",
        Local::now().format("%Y%m%d-%H%M%S")
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "Invalid filename."))?,
    );
    Ok((headers, Json(document)).into_response())
}

async fn import_jobs(
    State(state): State<Arc<AppState>>,
    Json(document): Json<JobsImport>,
) -> ApiResult<Response> {
    if let Some(format) = document.format.as_deref()
        && format != "datatask.jobs"
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Unsupported import format.",
        ));
    }
    if document.version.unwrap_or(1) != 1 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Unsupported import version.",
        ));
    }
    let on_conflict = document.on_conflict.as_deref().unwrap_or("skip");
    if !matches!(on_conflict, "skip" | "overwrite") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "on_conflict must be 'skip' or 'overwrite'.",
        ));
    }
    if document.jobs.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Import contains no jobs.",
        ));
    }
    if document.jobs.len() > 1000 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Import is limited to 1000 jobs.",
        ));
    }

    let _guard = state.repository_lock.lock().await;
    let mut prepared = Vec::with_capacity(document.jobs.len());
    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    for (index, portable) in document.jobs.iter().enumerate() {
        match portable_job_to_job(portable) {
            Ok(job) => {
                if !seen.insert(job.name.clone()) {
                    errors.push(json!({
                        "index": index,
                        "name": job.name,
                        "error": "duplicate job name in import.",
                    }));
                } else {
                    prepared.push(job);
                }
            }
            Err(error) => errors.push(json!({
                "index": index,
                "name": portable.name,
                "error": error,
            })),
        }
    }
    if !errors.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Import contains invalid jobs.",
                "errors": errors,
            })),
        )
            .into_response());
    }

    let mut created = Vec::new();
    let mut overwritten = Vec::new();
    let mut skipped = Vec::new();
    for job in prepared {
        let path = job_path(&state.config, &job.name)?;
        let exists = path.is_file();
        let job = if exists {
            if on_conflict == "skip" {
                skipped.push(job.name);
                continue;
            }
            let mut merged = job;
            if let Ok(existing) = read_job(&state.config, &merged.name) {
                merged.last_run = existing.last_run;
                merged.last_result = existing.last_result;
                merged.last_exit_code = existing.last_exit_code;
                merged.last_duration_ms = existing.last_duration_ms;
                merged.version = existing.version.saturating_add(1);
            }
            overwritten.push(merged.name.clone());
            merged
        } else {
            created.push(job.name.clone());
            job
        };
        if !document.dry_run {
            write_job(&state.config, &job)?;
        }
    }

    Ok(Json(json!({
        "dry_run": document.dry_run,
        "mode": on_conflict,
        "total": document.jobs.len(),
        "created": created,
        "overwritten": overwritten,
        "skipped": skipped,
    }))
    .into_response())
}

async fn get_jobs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<JobsQuery>,
) -> ApiResult<Response> {
    let jobs = list_jobs_cached(&state.config, &state.job_cache)?;
    let paginated = query.limit.is_some()
        || query.offset.is_some()
        || query.q.is_some()
        || query.status.is_some()
        || query.sort.is_some()
        || query.order.is_some();
    let jobs = prepare_jobs(jobs, &query)?;
    if !paginated {
        return Ok(Json(jobs).into_response());
    }
    let limit = query
        .limit
        .unwrap_or(DEFAULT_JOBS_LIMIT)
        .clamp(1, MAX_JOBS_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let total = jobs.len();
    let items: Vec<Job> = jobs.into_iter().skip(offset).take(limit).collect();
    Ok(Json(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
    .into_response())
}

async fn get_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    let job = read_job(&state.config, &name)?;
    Ok(job_response(StatusCode::OK, job))
}

async fn create_job(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateJob>,
) -> ApiResult<Response> {
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
    let retries = normalize_retries(payload.retries.unwrap_or(0))?;
    let retry_delay_seconds = normalize_retry_delay(payload.retry_delay_seconds.unwrap_or(0))?;
    let notify_url = normalize_notify_url(payload.notify_url.as_deref())?;
    let job = Job {
        name: payload.name.clone(),
        description: payload.description,
        datetime: payload.datetime.trim().to_owned(),
        action: payload.action,
        log: format!("logs/{}.log", payload.name),
        status,
        version: 1,
        last_run: None,
        working_dir,
        timeout_seconds,
        retries,
        retry_delay_seconds,
        notify_url,
        last_result: None,
        last_exit_code: None,
        last_duration_ms: None,
    };
    write_job(&state.config, &job)?;
    Ok(job_response(StatusCode::CREATED, job))
}

async fn update_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<UpdateJob>,
) -> ApiResult<Response> {
    let _guard = state.repository_lock.lock().await;
    if lock_running(&state).contains(&name) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Cannot edit a job while it is running.",
        ));
    }
    let mut job = read_job(&state.config, &name)?;
    if !if_match_allows(&headers, job.version)? {
        return Ok(job_conflict_response(job));
    }
    let mut changed = false;

    if let Some(description) = payload.description {
        validate_text_length("description", &description, MAX_DESCRIPTION_BYTES)?;
        if job.description != description {
            job.description = description;
            changed = true;
        }
    }
    if let Some(datetime) = payload.datetime {
        validate_schedule(&datetime)?;
        validate_text_length("datetime", &datetime, MAX_DATETIME_BYTES)?;
        let datetime = datetime.trim().to_owned();
        if job.datetime != datetime {
            job.datetime = datetime;
            changed = true;
        }
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
        if job.action != action {
            job.action = action;
            changed = true;
        }
    }
    if let Some(working_dir) = payload.working_dir {
        let working_dir = normalize_working_dir(Some(&working_dir))?;
        if job.working_dir != working_dir {
            job.working_dir = working_dir;
            changed = true;
        }
    }
    if let Some(timeout_seconds) = payload.timeout_seconds {
        let timeout_seconds = normalize_timeout(timeout_seconds)?;
        if job.timeout_seconds != timeout_seconds {
            job.timeout_seconds = timeout_seconds;
            changed = true;
        }
    }
    if let Some(retries) = payload.retries {
        let retries = normalize_retries(retries)?;
        if job.retries != retries {
            job.retries = retries;
            changed = true;
        }
    }
    if let Some(retry_delay_seconds) = payload.retry_delay_seconds {
        let retry_delay_seconds = normalize_retry_delay(retry_delay_seconds)?;
        if job.retry_delay_seconds != retry_delay_seconds {
            job.retry_delay_seconds = retry_delay_seconds;
            changed = true;
        }
    }
    if let Some(notify_url) = payload.notify_url {
        let notify_url = normalize_notify_url(Some(&notify_url))?;
        if job.notify_url != notify_url {
            job.notify_url = notify_url;
            changed = true;
        }
    }
    if let Some(status) = payload.status {
        let next_status = normalize_status(&status)?;
        if job.status != next_status {
            if job.status != "ativo" && next_status == "ativo" {
                clear_last_run_if_not_current_minute(&mut job);
            }
            job.status = next_status;
            changed = true;
        }
    }
    if changed {
        job.version = job.version.saturating_add(1);
        write_job(&state.config, &job)?;
    }
    Ok(job_response(StatusCode::OK, job))
}

async fn set_job_status(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    status: &'static str,
) -> ApiResult<Response> {
    let _guard = state.repository_lock.lock().await;
    if lock_running(&state).contains(&name) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Cannot change status while the job is running.",
        ));
    }
    let mut job = read_job(&state.config, &name)?;
    if job.status != status {
        if job.status != "ativo" && status == "ativo" {
            clear_last_run_if_not_current_minute(&mut job);
        }
        job.status = status.to_owned();
        job.version = job.version.saturating_add(1);
        write_job(&state.config, &job)?;
    }
    Ok(job_response(StatusCode::OK, job))
}

async fn activate_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    set_job_status(State(state), Path(name), "ativo").await
}

async fn deactivate_job(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Response> {
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
            job.version = job.version.saturating_add(1);
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

    let max_attempts = job.retries.saturating_add(1);
    let mut last_outcome = None;
    for attempt in 1..=max_attempts {
        // Acquire per attempt so a long backoff does not hold a concurrency slot.
        let _slot =
            state.job_slots.clone().acquire_owned().await.map_err(|_| {
                api_error(StatusCode::INTERNAL_SERVER_ERROR, "Job slots unavailable.")
            })?;
        let outcome = execute_command(&state, &job).await;
        drop(_slot);
        record_outcome(&state, name, &outcome).await;
        state.audit(json!({
            "ts": Local::now().to_rfc3339(),
            "event": "job.run",
            "job": name,
            "origin": origin.as_str(),
            "attempt": attempt,
            "max_attempts": max_attempts,
            "result": outcome.label(),
            "exit_code": outcome.exit_code,
            "duration_ms": outcome.duration.as_millis().min(u64::MAX as u128) as u64,
        }));
        state.record_history(RunRecord {
            timestamp: Local::now().to_rfc3339(),
            job: name.to_owned(),
            origin: origin.as_str().to_owned(),
            attempt,
            max_attempts,
            result: outcome.label().to_owned(),
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration.as_millis().min(u64::MAX as u128) as u64,
            timed_out: outcome.timed_out,
        });
        let success = outcome.success();
        last_outcome = Some(outcome);
        if success || attempt == max_attempts {
            break;
        }
        let delay = retry_delay(attempt, job.retry_delay_seconds);
        append_retry_notice(&state.config, name, attempt + 1, max_attempts, delay);
        time::sleep(delay).await;
    }
    let outcome = last_outcome.expect("run_job always performs at least one attempt");
    if !outcome.success() {
        notify_failure(&state, &job, &outcome, max_attempts).await;
    }

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
    state.metrics.observe(outcome);
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

fn append_retry_notice(
    config: &Config,
    name: &str,
    next_attempt: u32,
    max_attempts: u32,
    delay: Duration,
) {
    let path = config.logs_dir.join(format!("{name}.log"));
    let Ok(mut file) = OpenOptions::new().append(true).open(&path) else {
        return;
    };
    let _ = writeln!(
        file,
        "───── DataTask · retry {next_attempt}/{max_attempts} em {:.0}s ─────\n",
        delay.as_secs_f64()
    );
    let _ = file.flush();
}

fn notification_payload(job: &Job, outcome: &RunOutcome, attempts: u32) -> Value {
    json!({
        "event": "job.failed",
        "job": job.name,
        "description": job.description,
        "result": outcome.label(),
        "exit_code": outcome.exit_code,
        "timed_out": outcome.timed_out,
        "duration_ms": outcome.duration.as_millis().min(u64::MAX as u128) as u64,
        "attempts": attempts,
        "max_attempts": job.retries.saturating_add(1),
        "timestamp": Local::now().to_rfc3339(),
    })
}

async fn notify_failure(state: &AppState, job: &Job, outcome: &RunOutcome, attempts: u32) {
    let Some(url) = job.notify_url.as_deref() else {
        return;
    };
    let payload = notification_payload(job, outcome, attempts);
    match state.http_client.post(url).json(&payload).send().await {
        Ok(response) => {
            let status = response.status();
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "job.notify",
                "job": job.name,
                "status": status.as_u16(),
            }));
            if !status.is_success() {
                eprintln!(
                    "Webhook for '{}' returned HTTP {}.",
                    job.name,
                    status.as_u16()
                );
            }
        }
        Err(error) => {
            state.audit(json!({
                "ts": Local::now().to_rfc3339(),
                "event": "job.notify",
                "job": job.name,
                "error": error.to_string(),
            }));
            eprintln!("Webhook for '{}' failed: {error}", job.name);
        }
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
struct HistoryQuery {
    job: Option<String>,
    result: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Deserialize)]
struct JobsQuery {
    limit: Option<usize>,
    offset: Option<usize>,
    q: Option<String>,
    status: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

const DEFAULT_JOBS_LIMIT: usize = 50;
const MAX_JOBS_LIMIT: usize = 500;

fn prepare_jobs(mut jobs: Vec<Job>, query: &JobsQuery) -> ApiResult<Vec<Job>> {
    if let Some(status) = query.status.as_deref() {
        let status = normalize_status(status)?;
        jobs.retain(|job| job.status == status);
    }
    if let Some(term) = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty())
    {
        let term = term.to_lowercase();
        jobs.retain(|job| {
            job.name.to_lowercase().contains(&term)
                || job.description.to_lowercase().contains(&term)
        });
    }
    let sort = query.sort.as_deref().unwrap_or("name");
    if let Some(order) = query.order.as_deref()
        && !order.eq_ignore_ascii_case("asc")
        && !order.eq_ignore_ascii_case("desc")
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "order must be 'asc' or 'desc'.",
        ));
    }
    match sort {
        "name" => jobs.sort_by(|left, right| left.name.cmp(&right.name)),
        "schedule" => jobs.sort_by(|left, right| {
            left.datetime
                .cmp(&right.datetime)
                .then_with(|| left.name.cmp(&right.name))
        }),
        "status" => jobs.sort_by(|left, right| {
            left.status
                .cmp(&right.status)
                .then_with(|| left.name.cmp(&right.name))
        }),
        "last" => jobs.sort_by(|left, right| {
            left.last_run
                .cmp(&right.last_run)
                .then_with(|| left.name.cmp(&right.name))
        }),
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "sort must be name, schedule, status or last.",
            ));
        }
    }
    if query
        .order
        .as_deref()
        .is_some_and(|order| order.eq_ignore_ascii_case("desc"))
    {
        jobs.reverse();
    }
    Ok(jobs)
}
const DEFAULT_HISTORY_LIMIT: usize = 50;
const MAX_HISTORY_LIMIT: usize = 200;

async fn get_history(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let job = query.job.as_deref().filter(|value| !value.is_empty());
    if let Some(job) = job
        && !valid_job_name(job)
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Job name must contain only letters, numbers, '-' or '_'.",
        ));
    }
    let result = query
        .result
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    if let Some(result) = &result
        && !matches!(result.as_str(), "success" | "error" | "timeout")
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "result must be success, error or timeout.",
        ));
    }

    let Some(history) = &state.history else {
        return Ok(Json(json!({
            "enabled": false,
            "items": [],
            "total": 0,
            "limit": limit,
            "offset": offset,
        })));
    };
    let records = history.read();
    let (items, total) = filter_history(records, job, result.as_deref(), limit, offset);
    Ok(Json(json!({
        "enabled": true,
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
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
    let jobs = list_jobs_cached(&state.config, &state.job_cache)?;
    let mut running: Vec<String> = lock_running(&state).iter().cloned().collect();
    running.sort();
    let today = Local::now().format("%Y-%m-%d").to_string();
    let active_count = jobs.iter().filter(|job| job.status == "ativo").count();
    let recurring_count = jobs
        .iter()
        .filter(|job| job.datetime.split_whitespace().count() == 5)
        .count();
    let executed_today_count = jobs
        .iter()
        .filter(|job| {
            job.last_run
                .as_deref()
                .is_some_and(|last_run| last_run.starts_with(&today))
        })
        .count();
    let last_backup_verify = state
        .last_backup_verify
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Ok(Json(json!({
        "name": "DataTask",
        "version": env!("CARGO_PKG_VERSION"),
        "bind_address": state.config.bind_address,
        "port": state.config.port,
        "tls_terminated_by_proxy": state.config.tls_terminated_by_proxy,
        "native_tls": native_tls(&state.config),
        "trust_proxy_headers": state.config.trust_proxy_headers,
        "trusted_proxy_ips": state
            .config
            .trusted_proxy_ips
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "poll_interval_seconds": state.config.poll_interval_seconds,
        "command_timeout_seconds": state.config.command_timeout_seconds,
        "notification_timeout_seconds": state.config.notification_timeout_seconds,
        "max_concurrent_jobs": state.config.max_concurrent_jobs,
        "log_max_bytes": state.config.log_max_bytes,
        "rate_limit_per_minute": state.config.rate_limit_per_minute,
        "auth_failure_limit": state.config.auth_failure_limit,
        "auth_lockout_seconds": state.config.auth_lockout_seconds,
        "jobs_dir": state.config.jobs_dir.display().to_string(),
        "logs_dir": state.config.logs_dir.display().to_string(),
        "job_count": jobs.len(),
        "history_enabled": state.history.is_some(),
        "backup_enabled": state.config.backup_dir.is_some(),
        "backup_dir": state
            .config
            .backup_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        "backup_interval_seconds": state.config.backup_interval_seconds,
        "backup_retention": state.config.backup_retention,
        "backup_remote": if state.config.backup_ssh_target.is_some() {
            "ssh"
        } else if state.config.backup_s3_uri.is_some() {
            "s3"
        } else {
            ""
        },
        "backup_encryption": state.config.backup_encryption_key_file.is_some(),
        "backup_verify_interval_seconds": state.config.backup_verify_interval_seconds,
        "backup_alert_enabled": state.config.backup_alert_url.is_some(),
        "backup_last_verify": last_backup_verify,
        "active_count": active_count,
        "recurring_count": recurring_count,
        "executed_today_count": executed_today_count,
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
    let mut last_backup: Option<Instant> = None;
    let mut last_verify: Option<Instant> = None;
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
        if state.config.backup_dir.is_some() {
            let due = last_backup.is_none_or(|last| {
                last.elapsed() >= Duration::from_secs(state.config.backup_interval_seconds)
            });
            if due {
                last_backup = Some(Instant::now());
                let backup_state = state.clone();
                tokio::spawn(async move {
                    let lock = backup_state.backup_lock.clone();
                    if let Ok(_guard) = lock.try_lock() {
                        run_backup(backup_state).await;
                    }
                });
            }
        }
        let remote_configured =
            state.config.backup_ssh_target.is_some() || state.config.backup_s3_uri.is_some();
        if remote_configured && state.config.backup_verify_interval_seconds > 0 {
            let due = last_verify.is_none_or(|last| {
                last.elapsed() >= Duration::from_secs(state.config.backup_verify_interval_seconds)
            });
            if due {
                last_verify = Some(Instant::now());
                let verify_state = state.clone();
                tokio::spawn(async move {
                    let lock = verify_state.backup_lock.clone();
                    if let Ok(_guard) = lock.try_lock() {
                        run_remote_verify(verify_state).await;
                    }
                });
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
    if let Some(history) = &config.history_file {
        if let Some(parent) = history.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
        for path in [history.clone(), {
            let mut rotated = history.as_os_str().to_owned();
            rotated.push(".1");
            PathBuf::from(rotated)
        }] {
            if path.is_file() {
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            }
        }
    }
    if let Some(backup_dir) = &config.backup_dir {
        fs::create_dir_all(backup_dir)?;
        let _ = fs::set_permissions(backup_dir, fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(backup_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
                }
            }
        }
    }
    for name in ["backup-verify.jsonl", "backup-verify.jsonl.1"] {
        let path = config.logs_dir.join(name);
        if path.is_file() {
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
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

async fn resolve_bind_address(address: &str) -> io::Result<SocketAddr> {
    if let Ok(address) = address.parse() {
        return Ok(address);
    }
    tokio::net::lookup_host(address)
        .await?
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Cannot resolve bind address {address}."),
            )
        })
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
    let _instance_lock = InstanceLock::acquire(&config.jobs_dir.join(".datatask.lock"))?;

    let config = Arc::new(config);
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.notification_timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("DataTask/", env!("CARGO_PKG_VERSION")))
        .build()?;
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
    let history = config
        .history_file
        .clone()
        .map(|path| Arc::new(HistoryLog::new(path)));
    let state = Arc::new(AppState {
        config: config.clone(),
        repository_lock: Arc::new(Mutex::new(())),
        job_cache: Arc::new(JobCache::new()),
        running_jobs: Arc::new(StdMutex::new(HashSet::new())),
        process_groups: Arc::new(StdMutex::new(HashSet::new())),
        job_slots: Arc::new(Semaphore::new(config.max_concurrent_jobs)),
        backup_lock: Arc::new(Mutex::new(())),
        token_digest: digest,
        guard,
        audit,
        history,
        last_tick: Arc::new(StdMutex::new(None)),
        last_backup_verify: Arc::new(StdMutex::new(None)),
        last_backup_alert: Arc::new(StdMutex::new(None)),
        verify_history: Arc::new(VerifyHistoryLog::new(
            config.logs_dir.join("backup-verify.jsonl"),
        )),
        backup_health: Arc::new(StdMutex::new(BackupHealth::default())),
        metrics: Arc::new(Metrics::new()),
        http_client,
    });
    let scheduler_handle = tokio::spawn(scheduler(state.clone()));

    let api: Router = Router::new()
        .route("/api/info", get(get_info))
        .route("/api/history", get(get_history))
        .route("/api/backups", get(get_backups))
        .route("/api/backups/health", get(get_backup_health))
        .route("/api/backups/verifications", get(get_backup_verifications))
        .route("/api/backups/verify", post(verify_backups))
        .route("/api/backups/{name}", get(get_backup_file))
        .route("/api/jobs", get(get_jobs).post(create_job))
        .route("/api/jobs/export", get(export_jobs))
        .route("/api/jobs/import", post(import_jobs))
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
        .route("/metrics", get(metrics))
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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), api_guard))
        .layer(middleware::from_fn_with_state(state.clone(), https_guard));

    let address = format!("{}:{}", config.bind_address, config.port);
    if native_tls(&config) {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let cert = config
            .tls_cert_file
            .clone()
            .expect("native_tls guarantees a certificate path");
        let key = config
            .tls_key_file
            .clone()
            .expect("native_tls guarantees a key path");
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
            .await
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Cannot load TLS certificate/key: {error}"),
                )
            })?;
        let reload_tls = tls.clone();
        let reload_cert = cert.clone();
        let reload_key = key.clone();
        tokio::spawn(async move {
            let mut signal =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
                    Ok(signal) => signal,
                    Err(error) => {
                        eprintln!("Cannot listen for SIGHUP TLS reloads: {error}");
                        return;
                    }
                };
            loop {
                signal.recv().await;
                match reload_tls
                    .reload_from_pem_file(&reload_cert, &reload_key)
                    .await
                {
                    Ok(()) => eprintln!("TLS certificate and key reloaded."),
                    Err(error) => eprintln!("Cannot reload TLS certificate/key: {error}"),
                }
            }
        });
        let socket = resolve_bind_address(&address).await?;
        let handle: axum_server::Handle<SocketAddr> = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(10)));
        });
        println!("DataTask API listening on https://{address}");
        axum_server::bind_rustls(socket, tls)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&address).await?;
        println!("DataTask API listening on http://{address}");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    }

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

    #[test]
    fn retries_use_exponential_backoff_with_cap() {
        assert_eq!(retry_delay(1, 5), Duration::from_secs(5));
        assert_eq!(retry_delay(2, 5), Duration::from_secs(10));
        assert_eq!(retry_delay(3, 5), Duration::from_secs(20));
        assert_eq!(retry_delay(10, 5), Duration::from_secs(2_560));
        assert_eq!(
            retry_delay(20, 5),
            Duration::from_secs(MAX_RETRY_DELAY_SECONDS)
        );
        assert_eq!(retry_delay(1, 0), Duration::from_secs(1));
    }

    #[test]
    fn validates_retry_settings_and_webhook_url() {
        assert_eq!(normalize_retries(0).unwrap(), 0);
        assert_eq!(normalize_retries(MAX_RETRIES).unwrap(), MAX_RETRIES);
        assert!(normalize_retries(MAX_RETRIES + 1).is_err());
        assert_eq!(normalize_retry_delay(0).unwrap(), 5);
        assert_eq!(normalize_retry_delay(3).unwrap(), 3);
        assert!(normalize_retry_delay(MAX_RETRY_DELAY_SECONDS + 1).is_err());
        assert_eq!(normalize_notify_url(Some("")).unwrap(), None);
        assert_eq!(
            normalize_notify_url(Some("https://example.com/hook")).unwrap(),
            Some("https://example.com/hook".to_owned())
        );
        assert!(normalize_notify_url(Some("ftp://example.com/hook")).is_err());
        assert!(normalize_notify_url(Some("not a url")).is_err());
        assert!(normalize_notify_url(Some("https://user:pass@example.com")).is_err());
    }

    #[test]
    fn notification_payload_never_includes_action_or_log() {
        let job = Job {
            name: "backup".to_owned(),
            description: "Rotina".to_owned(),
            datetime: "* * * * *".to_owned(),
            action: "secret-command".to_owned(),
            log: "logs/backup.log".to_owned(),
            status: "ativo".to_owned(),
            version: 1,
            last_run: None,
            working_dir: None,
            timeout_seconds: None,
            retries: 2,
            retry_delay_seconds: 5,
            notify_url: Some("https://example.com/hook".to_owned()),
            last_result: None,
            last_exit_code: None,
            last_duration_ms: None,
        };
        let outcome = RunOutcome {
            exit_code: Some(2),
            timed_out: false,
            duration: Duration::from_millis(250),
            error: None,
        };
        let payload = notification_payload(&job, &outcome, 3);
        assert_eq!(payload["event"], "job.failed");
        assert_eq!(payload["job"], "backup");
        assert_eq!(payload["result"], "error");
        assert_eq!(payload["attempts"], 3);
        assert_eq!(payload["max_attempts"], 3);
        assert!(payload.get("action").is_none());
        assert!(payload.get("log").is_none());
        assert!(!payload.to_string().contains("secret-command"));
    }

    #[test]
    fn filters_and_paginates_history_newest_first() {
        let record = |job: &str, attempt: u32, result: &str| RunRecord {
            timestamp: format!("2026-11-01T00:00:0{attempt}Z"),
            job: job.to_owned(),
            origin: "manual".to_owned(),
            attempt,
            max_attempts: 3,
            result: result.to_owned(),
            exit_code: Some(0),
            duration_ms: 10,
            timed_out: false,
        };
        let records = vec![
            record("alpha", 1, "error"),
            record("alpha", 2, "success"),
            record("beta", 1, "timeout"),
        ];
        let (items, total) = filter_history(records.clone(), Some("alpha"), None, 1, 0);
        assert_eq!(total, 2);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].attempt, 2);
        assert_eq!(items[0].result, "success");

        let (items, total) = filter_history(records, None, Some("timeout"), 10, 0);
        assert_eq!(total, 1);
        assert_eq!(items[0].job, "beta");
    }

    #[test]
    fn appends_and_reads_history_jsonl() {
        let directory = std::env::temp_dir().join(format!(
            "datatask-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let history = HistoryLog::new(directory.join("history.jsonl"));
        let record = RunRecord {
            timestamp: "2026-11-01T00:00:00Z".to_owned(),
            job: "alpha".to_owned(),
            origin: "scheduler".to_owned(),
            attempt: 1,
            max_attempts: 1,
            result: "success".to_owned(),
            exit_code: Some(0),
            duration_ms: 42,
            timed_out: false,
        };
        history.append(&record);
        history.append(&record);
        let records = history.read();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].job, "alpha");
        assert_eq!(records[0].duration_ms, 42);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn validates_backup_names_and_checksums() {
        assert!(valid_backup_name("datatask-jobs-20260101-000000.json"));
        assert!(!valid_backup_name("../datatask-jobs-x.json"));
        assert!(!valid_backup_name("other.json"));
        assert_eq!(sha256_hex(b"abc").len(), 64);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn appends_and_reads_backup_verification_history() {
        let directory = std::env::temp_dir().join(format!(
            "datatask-verify-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let log = VerifyHistoryLog::new(directory.join("backup-verify.jsonl"));
        let summary = BackupVerifySummary {
            timestamp: "2026-11-06T00:00:00Z".to_owned(),
            checked: 2,
            ok: 1,
            failed: 1,
            items: vec![
                BackupVerifyItem {
                    name: "a.json".to_owned(),
                    ok: true,
                    error: None,
                },
                BackupVerifyItem {
                    name: "b.json".to_owned(),
                    ok: false,
                    error: Some("checksum mismatch".to_owned()),
                },
            ],
        };
        log.append(&summary);
        log.append(&summary);
        let entries = log.read();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].failed, 1);
        assert_eq!(entries[0].items[1].name, "b.json");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn builds_deduplicated_backup_alert_keys() {
        let summary = |checked: usize, failed_items: Vec<(&str, bool)>| BackupVerifySummary {
            timestamp: "2026-11-06T00:00:00Z".to_owned(),
            checked,
            ok: failed_items.iter().filter(|(_, ok)| *ok).count(),
            failed: failed_items.iter().filter(|(_, ok)| !*ok).count(),
            items: failed_items
                .into_iter()
                .map(|(name, ok)| BackupVerifyItem {
                    name: name.to_owned(),
                    ok,
                    error: (!ok).then(|| "checksum mismatch".to_owned()),
                })
                .collect(),
        };
        assert_eq!(
            backup_alert_key(&summary(0, vec![])),
            Some("missing".to_owned())
        );
        assert_eq!(
            backup_alert_key(&summary(2, vec![("b.json", true), ("a.json", false)])),
            Some("failed:a.json".to_owned())
        );
        assert_eq!(backup_alert_key(&summary(1, vec![("a.json", true)])), None);
    }

    #[test]
    fn selects_old_remote_artifacts_for_pruning() {
        let names = vec![
            "datatask-jobs-20260101-000000.json".to_owned(),
            "datatask-jobs-20260101-000000.json.sha256".to_owned(),
            "datatask-jobs-20260102-000000.json.enc".to_owned(),
            "datatask-jobs-20260102-000000.json.enc.sha256".to_owned(),
            "datatask-jobs-20260103-000000.json".to_owned(),
            "unrelated.txt".to_owned(),
        ];
        assert_eq!(
            remote_artifacts_to_prune(names, 2),
            vec!["datatask-jobs-20260101-000000.json".to_owned()]
        );
    }

    #[test]
    fn parses_remote_backup_targets() {
        assert_eq!(
            split_ssh_target("user@host:/srv/backups/"),
            Some(("user@host", "/srv/backups"))
        );
        assert_eq!(split_ssh_target("host:/tmp"), Some(("host", "/tmp")));
        assert_eq!(split_ssh_target("host"), None);
        assert_eq!(split_ssh_target("host:"), None);
        assert_eq!(shell_quote("/tmp/a b's"), "'/tmp/a b'\\''s'");
    }

    #[test]
    fn writes_and_prunes_automatic_backups() {
        let directory = std::env::temp_dir().join(format!(
            "datatask-backup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        for stamp in ["20260101-000000", "20260102-000000", "20260103-000000"] {
            fs::write(directory.join(format!("datatask-jobs-{stamp}.json")), "{}").unwrap();
        }
        fs::write(directory.join("unrelated.txt"), "keep").unwrap();
        let (path, checksum) = write_backup(&directory, 2, &[]).unwrap();
        assert_eq!(checksum.len(), 64);
        let contents = fs::read_to_string(&path).unwrap();
        let document: Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(document["format"], "datatask.jobs");
        assert_eq!(document["jobs"].as_array().unwrap().len(), 0);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mut checksum_path = path.as_os_str().to_owned();
        checksum_path.push(".sha256");
        let checksum_path = PathBuf::from(checksum_path);
        let checksum_text = fs::read_to_string(&checksum_path).unwrap();
        assert!(checksum_text.starts_with(&checksum));

        let mut backups: Vec<String> = fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with("datatask-jobs-") && name.ends_with(".json"))
            .collect();
        backups.sort();
        assert_eq!(backups.len(), 2);
        assert!(directory.join("unrelated.txt").is_file());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn validates_portable_jobs_for_import() {
        let portable = PortableJob {
            name: "imported".to_owned(),
            description: "teste".to_owned(),
            datetime: "* * * * *".to_owned(),
            action: "exit 0".to_owned(),
            status: "ativo".to_owned(),
            working_dir: None,
            timeout_seconds: Some(30),
            retries: 2,
            retry_delay_seconds: 5,
            notify_url: Some("https://example.com/hook".to_owned()),
        };
        let job = portable_job_to_job(&portable).unwrap();
        assert_eq!(job.name, "imported");
        assert_eq!(job.version, 1);
        assert_eq!(job.timeout_seconds, Some(30));
        assert_eq!(job.retries, 2);

        let mut invalid = portable;
        invalid.name = "../escape".to_owned();
        assert!(portable_job_to_job(&invalid).is_err());
        invalid.name = "valid".to_owned();
        invalid.action = "   ".to_owned();
        assert!(portable_job_to_job(&invalid).is_err());
    }

    #[test]
    fn filters_and_sorts_jobs_server_side() {
        let job = |name: &str, description: &str, status: &str, last_run: Option<&str>| -> Job {
            serde_json::from_value(json!({
                "name": name,
                "description": description,
                "datetime": "* * * * *",
                "action": "exit 0",
                "log": format!("logs/{name}.log"),
                "status": status,
                "last_run": last_run,
            }))
            .unwrap()
        };
        let jobs = vec![
            job("alpha", "backup diário", "ativo", Some("2026-11-01 10:00")),
            job("beta", "limpeza", "desativo", None),
            job("gamma", "backup semanal", "ativo", Some("2026-11-02 10:00")),
        ];
        let query = JobsQuery {
            limit: None,
            offset: None,
            q: Some("backup".to_owned()),
            status: Some("ativo".to_owned()),
            sort: Some("last".to_owned()),
            order: Some("desc".to_owned()),
        };
        let selected = prepare_jobs(jobs, &query).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|job| job.name.as_str())
                .collect::<Vec<_>>(),
            vec!["gamma", "alpha"]
        );
    }

    #[test]
    fn job_cache_invalidates_when_files_change() {
        let directory = std::env::temp_dir().join(format!(
            "datatask-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let jobs_dir = directory.join("jobs");
        let logs_dir = directory.join("logs");
        fs::create_dir_all(&jobs_dir).unwrap();
        fs::create_dir_all(&logs_dir).unwrap();
        let config_path = directory.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "bind_address = \"127.0.0.1\"\nport = 8080\napi_token = \"{}\"\njobs_dir = \"{}\"\nlogs_dir = \"{}\"\npoll_interval_seconds = 1\ncommand_timeout_seconds = 5\n",
                "0123456789abcdef".repeat(4),
                jobs_dir.display(),
                logs_dir.display()
            ),
        )
        .unwrap();
        let _ = fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600));
        let (config, _) = load_config(&config_path).unwrap();
        let cache = JobCache::new();
        let job = Job {
            name: "cached".to_owned(),
            description: "original".to_owned(),
            datetime: "* * * * *".to_owned(),
            action: "exit 0".to_owned(),
            log: "logs/cached.log".to_owned(),
            status: "ativo".to_owned(),
            version: 1,
            last_run: None,
            working_dir: None,
            timeout_seconds: None,
            retries: 0,
            retry_delay_seconds: 5,
            notify_url: None,
            last_result: None,
            last_exit_code: None,
            last_duration_ms: None,
        };
        write_job(&config, &job).unwrap();
        let first = list_jobs_cached(&config, &cache).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].description, "original");
        assert_eq!(list_jobs_cached(&config, &cache).unwrap().len(), 1);

        let mut changed = job;
        changed.description = "changed-description".to_owned();
        write_job(&config, &changed).unwrap();
        let second = list_jobs_cached(&config, &cache).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].description, "changed-description");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn prevents_two_instances_from_sharing_jobs() {
        let directory = std::env::temp_dir().join(format!(
            "datatask-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(".datatask.lock");
        let first = InstanceLock::acquire(&path).unwrap();
        assert!(InstanceLock::acquire(&path).is_err());
        drop(first);
        assert!(InstanceLock::acquire(&path).is_ok());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn parses_if_match_versions() {
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::IF_MATCH, HeaderValue::from_str(value).unwrap());
            headers
        };
        assert!(if_match_allows(&HeaderMap::new(), 1).unwrap());
        assert!(if_match_allows(&header("\"1\""), 1).unwrap());
        assert!(!if_match_allows(&header("\"2\""), 1).unwrap());
        assert!(if_match_allows(&header("W/\"1\""), 1).unwrap());
        assert!(if_match_allows(&header("\"1\", \"2\""), 2).unwrap());
        assert!(if_match_allows(&header("*"), 99).unwrap());
        assert!(if_match_allows(&header("not-a-version"), 1).is_err());
    }

    #[test]
    fn renders_prometheus_metrics() {
        let metrics = Metrics::new();
        for outcome in [
            RunOutcome {
                exit_code: Some(0),
                timed_out: false,
                duration: Duration::from_millis(50),
                error: None,
            },
            RunOutcome {
                exit_code: Some(2),
                timed_out: false,
                duration: Duration::from_millis(1_500),
                error: None,
            },
            RunOutcome {
                exit_code: None,
                timed_out: true,
                duration: Duration::from_secs(10),
                error: None,
            },
        ] {
            metrics.observe(&outcome);
        }
        let text = metrics.render(3, 2, 1, true, Some(0.25));
        assert!(text.contains("datatask_job_runs_total{result=\"success\"} 1"));
        assert!(text.contains("datatask_job_runs_total{result=\"error\"} 1"));
        assert!(text.contains("datatask_job_runs_total{result=\"timeout\"} 1"));
        assert!(text.contains("datatask_job_duration_seconds_bucket{le=\"0.1\"} 1"));
        assert!(text.contains("datatask_job_duration_seconds_bucket{le=\"1\"} 1"));
        assert!(text.contains("datatask_job_duration_seconds_bucket{le=\"5\"} 2"));
        assert!(text.contains("datatask_job_duration_seconds_bucket{le=\"15\"} 3"));
        assert!(text.contains("datatask_job_duration_seconds_bucket{le=\"+Inf\"} 3"));
        assert!(text.contains("datatask_job_duration_seconds_count 3"));
        assert!(text.contains("datatask_jobs_total 3"));
        assert!(text.contains("datatask_jobs_active 2"));
        assert!(text.contains("datatask_jobs_running 1"));
        assert!(text.contains("datatask_scheduler_ready 1"));
        assert!(text.contains("datatask_scheduler_lag_seconds 0.250000"));
    }

    #[test]
    fn enforces_loopback_or_tls_proxy() {
        assert!(bind_is_loopback("127.0.0.1"));
        assert!(bind_is_loopback("127.0.0.2"));
        assert!(bind_is_loopback("::1"));
        assert!(bind_is_loopback("localhost"));
        assert!(!bind_is_loopback("0.0.0.0"));
        assert!(!bind_is_loopback("::"));
        assert!(!bind_is_loopback("192.168.0.10"));

        let proxy: IpAddr = "10.0.0.10".parse().unwrap();
        let other: IpAddr = "10.0.0.11".parse().unwrap();
        assert!(is_trusted_proxy(IpAddr::V4(Ipv4Addr::LOCALHOST), &[proxy]));
        assert!(is_trusted_proxy(proxy, &[proxy]));
        assert!(!is_trusted_proxy(other, &[proxy]));

        assert_eq!(
            https_decision(false, "/api/jobs", false, false, false),
            HttpsDecision::Allow
        );
        assert_eq!(
            https_decision(true, "/health", false, false, false),
            HttpsDecision::Allow
        );
        assert_eq!(
            https_decision(true, "/readyz", false, false, false),
            HttpsDecision::Allow
        );
        assert_eq!(
            https_decision(true, "/api/jobs", true, true, false),
            HttpsDecision::Allow
        );
        assert_eq!(
            https_decision(true, "/api/jobs", false, false, true),
            HttpsDecision::UntrustedProxy
        );
        assert_eq!(
            https_decision(true, "/api/jobs", false, true, false),
            HttpsDecision::UpgradeRequired
        );
        assert_eq!(
            https_decision(true, "/api/jobs", false, true, true),
            HttpsDecision::Allow
        );
    }
}
