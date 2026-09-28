//! The job manager: starts scans, cleans and app work on blocking threads, samples their
//! progress into `job` events, and retains their outputs until the UI releases them.
//!
//! Output size: every output is sent as one frame, bounded by
//! [`omc_ipc::frame::MAX_FRAME_LEN`]. The scanners cap their lists (and report
//! `truncated`), so outputs fit; should one still not, the connection's writer answers that
//! request with an `internal` error instead of dropping the connection (see
//! `engine::write_frames`), so no size check (which would serialize twice) runs here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use omc_apps::{AppFiles, AppRecord, StartupRecord};
use omc_proto::apps::{AppList, StartupChange, StartupItem, StartupList, UninstallReport};
use omc_proto::files::{DupReport, FileReport, SpaceListing};
use omc_proto::jobs::{
    CleanReport, CleanSpec, ItemId, JobId, JobKind, JobOutput, JobSpec, JobState, JobStatus,
    JobUpdate, Progress, ScanArea, UninstallSpec,
};
use omc_proto::junk::JunkReport;
use omc_proto::settings::CleanSettings;
use omc_proto::{ErrorCode, Event, RpcError};
use omc_scan::junk::JunkArea;
use omc_scan::space::SpaceTree;
use omc_scan::{JobCtx, Scanned, WalkOptions, Walker};
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::Engine;

/// Finished jobs kept; older finished ones are evicted (running jobs never are).
const MAX_FINISHED: usize = 64;
/// Progress sampling period (the wire promises at most ~10 updates per second per job).
const SAMPLE_EVERY: Duration = Duration::from_millis(100);

/// What a finished job keeps for `job_result`, `space_children` and follow-up jobs.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "always behind an `Arc`, one per retained job"
)]
enum Retained {
    Junk(Scanned<JunkReport>),
    Files(Scanned<FileReport>),
    Dupes(Scanned<DupReport>),
    Space(SpaceTree),
    Clean(CleanReport),
    Apps(Vec<AppRecord>),
    AppFiles(AppFiles),
    Uninstall(UninstallReport),
    Startup(Vec<StartupRecord>),
    StartupChanged(Option<StartupItem>),
}

impl Retained {
    /// The wire form (for a space tree: its root listing).
    fn output(&self) -> Result<JobOutput, RpcError> {
        Ok(match self {
            Self::Junk(scan) => JobOutput::Junk(scan.report.clone()),
            Self::Files(scan) => JobOutput::Files(scan.report.clone()),
            Self::Dupes(scan) => JobOutput::Duplicates(scan.report.clone()),
            Self::Space(tree) => JobOutput::Space(
                tree.listing(0)
                    .ok_or_else(|| RpcError::new(ErrorCode::Internal, "space tree has no root"))?,
            ),
            Self::Clean(report) => JobOutput::Clean(report.clone()),
            Self::Apps(apps) => JobOutput::Apps(AppList {
                apps: apps.iter().map(|app| app.info.clone()).collect(),
            }),
            Self::AppFiles(files) => JobOutput::AppFiles(files.report.clone()),
            Self::Uninstall(report) => JobOutput::Uninstall(report.clone()),
            Self::Startup(items) => JobOutput::Startup(StartupList {
                items: items.iter().map(|record| record.item.clone()).collect(),
            }),
            Self::StartupChanged(item) => JobOutput::StartupChanged(item.clone()),
        })
    }
}

/// One row of the job table.
#[derive(Debug)]
struct JobEntry {
    kind: JobKind,
    ctx: Arc<JobCtx>,
    state: JobState,
    output: Option<Arc<Retained>>,
    /// Finish order, for eviction (0 while running).
    finished: u64,
}

/// The job table.
#[derive(Debug)]
pub(crate) struct Jobs {
    next_id: AtomicU64,
    finish_seq: AtomicU64,
    table: Mutex<HashMap<JobId, JobEntry>>,
    /// One permit: mutating jobs (clean, uninstall, startup change) run one at a time.
    mutating: Arc<Semaphore>,
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            finish_seq: AtomicU64::new(1),
            table: Mutex::new(HashMap::new()),
            mutating: Arc::new(Semaphore::new(1)),
        }
    }
}

fn not_found(job: JobId) -> RpcError {
    RpcError::new(ErrorCode::NotFound, format!("no job {job}"))
}

fn bad_request(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::BadRequest, message)
}

fn internal(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::Internal, message)
}

impl Jobs {
    /// State and progress of `job`.
    pub(crate) fn status(&self, job: JobId) -> Result<JobStatus, RpcError> {
        let table = self.table.lock();
        let entry = table.get(&job).ok_or_else(|| not_found(job))?;
        Ok(JobStatus {
            state: entry.state.clone(),
            progress: entry.ctx.snapshot(),
        })
    }

    /// The output of a finished job.
    pub(crate) fn result(&self, job: JobId) -> Result<JobOutput, RpcError> {
        self.finished_output(job)?.output()
    }

    /// Asks a running job to stop; a no-op for finished jobs.
    pub(crate) fn cancel(&self, job: JobId) -> Result<(), RpcError> {
        let table = self.table.lock();
        let entry = table.get(&job).ok_or_else(|| not_found(job))?;
        if !entry.state.is_finished() {
            tracing::info!(job, "job cancel requested");
            entry.ctx.cancel();
        }
        Ok(())
    }

    /// Forgets a finished job and its output.
    pub(crate) fn release(&self, job: JobId) -> Result<(), RpcError> {
        let mut table = self.table.lock();
        let entry = table.get(&job).ok_or_else(|| not_found(job))?;
        if !entry.state.is_finished() {
            return Err(bad_request(format!("job {job} is still running")));
        }
        table.remove(&job);
        Ok(())
    }

    /// One directory level of a finished space-lens job.
    pub(crate) fn space_children(
        &self,
        job: JobId,
        node: ItemId,
    ) -> Result<SpaceListing, RpcError> {
        let Retained::Space(tree) = &*self.finished_output(job)? else {
            return Err(bad_request(format!("job {job} is not a space-lens scan")));
        };
        tree.listing(node).ok_or_else(|| {
            RpcError::new(
                ErrorCode::NotFound,
                format!("job {job} has no directory node {node}"),
            )
        })
    }

    /// Takes the mutation permit, as a running clean would (tests queue jobs behind it).
    #[cfg(test)]
    pub(crate) fn hold_mutation(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.mutating).try_acquire_owned().ok()
    }

    /// The retained output of a finished job.
    fn finished_output(&self, job: JobId) -> Result<Arc<Retained>, RpcError> {
        let table = self.table.lock();
        let entry = table.get(&job).ok_or_else(|| not_found(job))?;
        if !entry.state.is_finished() {
            return Err(bad_request(format!("job {job} is still running")));
        }
        entry.output.clone().ok_or_else(|| {
            bad_request(format!(
                "job {job} ended without output ({:?})",
                entry.state
            ))
        })
    }

    /// Registers a running job.
    fn insert(&self, kind: JobKind) -> (JobId, Arc<JobCtx>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ctx = Arc::new(JobCtx::new());
        self.table.lock().insert(
            id,
            JobEntry {
                kind,
                ctx: Arc::clone(&ctx),
                state: JobState::Running,
                output: None,
                finished: 0,
            },
        );
        (id, ctx)
    }

    /// Records the end of a job and evicts the oldest finished jobs beyond the limit.
    fn finish(&self, job: JobId, state: JobState, output: Option<Retained>) {
        let seq = self.finish_seq.fetch_add(1, Ordering::Relaxed);
        let mut table = self.table.lock();
        if let Some(entry) = table.get_mut(&job) {
            entry.state = state;
            entry.output = output.map(Arc::new);
            entry.finished = seq;
        }
        while table.values().filter(|e| e.state.is_finished()).count() > MAX_FINISHED {
            let Some(oldest) = table
                .iter()
                .filter(|(_, e)| e.state.is_finished())
                .min_by_key(|(_, e)| e.finished)
                .map(|(id, e)| (*id, e.kind))
            else {
                break;
            };
            tracing::debug!(job = oldest.0, kind = ?oldest.1, "evicting finished job");
            table.remove(&oldest.0);
        }
    }
}

/// A validated job, ready to run on a blocking thread.
enum Work {
    Junk(JunkArea),
    Leftovers,
    Large,
    Dupes,
    Space(PathBuf),
    Clean {
        source: Arc<Retained>,
        items: Vec<ItemId>,
    },
    ListApps,
    AppFiles(Box<AppRecord>),
    Uninstall {
        source: Arc<Retained>,
        items: Vec<ItemId>,
        run_uninstaller: bool,
    },
    ListStartup,
    ChangeStartup {
        record: Box<StartupRecord>,
        change: StartupChange,
    },
}

impl Work {
    /// Changes the system, so it waits for the one mutation permit.
    const fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Clean { .. } | Self::Uninstall { .. } | Self::ChangeStartup { .. }
        )
    }
}

/// Why a job produced no output.
enum Fail {
    Cancelled,
    Failed(String),
}

impl From<omc_scan::Error> for Fail {
    fn from(err: omc_scan::Error) -> Self {
        match err {
            omc_scan::Error::Cancelled => Self::Cancelled,
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<omc_apps::Error> for Fail {
    fn from(err: omc_apps::Error) -> Self {
        match err {
            omc_apps::Error::Cancelled | omc_apps::Error::Scan(omc_scan::Error::Cancelled) => {
                Self::Cancelled
            }
            other => Self::Failed(other.to_string()),
        }
    }
}

impl Engine {
    /// Validates `spec`, registers the job and starts it. Answers before any work is done;
    /// only validation that needs the disk (a space-lens root) runs on a blocking thread.
    pub(crate) async fn start_job(&self, spec: JobSpec) -> Result<JobId, RpcError> {
        let kind = spec.kind();
        let work = self.prepare(spec).await?;
        let settings = self.settings().get().clean;
        let (id, ctx) = self.jobs().insert(kind);
        tracing::info!(job = id, ?kind, "job started");
        tokio::spawn(drive(self.clone(), id, kind, ctx, work, settings));
        Ok(id)
    }

    async fn prepare(&self, spec: JobSpec) -> Result<Work, RpcError> {
        let jobs = self.jobs();
        Ok(match spec {
            JobSpec::Scan(area) => match area {
                ScanArea::SystemJunk => Work::Junk(JunkArea::System),
                ScanArea::BrowserData => Work::Junk(JunkArea::Browser),
                ScanArea::DeveloperJunk => Work::Junk(JunkArea::Developer),
                ScanArea::Trash => Work::Junk(JunkArea::Trash),
                ScanArea::Installers => Work::Junk(JunkArea::Installers),
                ScanArea::Leftovers => Work::Leftovers,
                ScanArea::LargeOldFiles => Work::Large,
                ScanArea::Duplicates => Work::Dupes,
                ScanArea::SpaceLens { root } => Work::Space(space_root(&root).await?),
            },
            JobSpec::Clean(CleanSpec { scan_job, items }) => {
                let source = jobs.finished_output(scan_job)?;
                if !matches!(
                    &*source,
                    Retained::Junk(_)
                        | Retained::Files(_)
                        | Retained::Dupes(_)
                        | Retained::Space(_)
                ) {
                    return Err(bad_request(format!("job {scan_job} is not a scan")));
                }
                Work::Clean { source, items }
            }
            JobSpec::ListApps => Work::ListApps,
            JobSpec::AppFiles { apps_job, app } => {
                let Retained::Apps(apps) = &*jobs.finished_output(apps_job)? else {
                    return Err(bad_request(format!(
                        "job {apps_job} is not a list_apps job"
                    )));
                };
                let record = apps.iter().find(|a| a.info.id == app).ok_or_else(|| {
                    RpcError::new(
                        ErrorCode::NotFound,
                        format!("job {apps_job} has no app {app}"),
                    )
                })?;
                Work::AppFiles(Box::new(record.clone()))
            }
            JobSpec::Uninstall(UninstallSpec {
                files_job,
                items,
                run_uninstaller,
            }) => {
                let source = jobs.finished_output(files_job)?;
                if !matches!(&*source, Retained::AppFiles(_)) {
                    return Err(bad_request(format!(
                        "job {files_job} is not an app_files job"
                    )));
                }
                Work::Uninstall {
                    source,
                    items,
                    run_uninstaller,
                }
            }
            JobSpec::ListStartup => Work::ListStartup,
            JobSpec::ChangeStartup {
                list_job,
                item,
                change,
            } => {
                let Retained::Startup(records) = &*jobs.finished_output(list_job)? else {
                    return Err(bad_request(format!(
                        "job {list_job} is not a list_startup job"
                    )));
                };
                let record = records.iter().find(|r| r.item.id == item).ok_or_else(|| {
                    RpcError::new(
                        ErrorCode::NotFound,
                        format!("job {list_job} has no item {item}"),
                    )
                })?;
                Work::ChangeStartup {
                    record: Box::new(record.clone()),
                    change,
                }
            }
        })
    }
}

/// A space-lens root must be an absolute, existing directory.
async fn space_root(root: &str) -> Result<PathBuf, RpcError> {
    let path = omc_scan::paths::normalize(&PathBuf::from(root))
        .ok_or_else(|| bad_request(format!("space-lens root {root:?} is not an absolute path")))?;
    let checked = tokio::task::spawn_blocking(move || path.is_dir().then_some(path))
        .await
        .map_err(|err| internal(format!("checking the space-lens root: {err}")))?;
    checked.ok_or_else(|| bad_request(format!("space-lens root {root:?} is not a directory")))
}

/// Runs one job to its end: waits for the mutation permit when needed, runs the work on a
/// blocking thread, samples progress into `job` events and records the outcome.
async fn drive(
    engine: Engine,
    id: JobId,
    kind: JobKind,
    ctx: Arc<JobCtx>,
    work: Work,
    settings: CleanSettings,
) {
    let permits = work
        .is_mutating()
        .then(|| Arc::clone(&engine.jobs().mutating));
    let worker_ctx = Arc::clone(&ctx);
    let task = async move {
        let _permit = match permits {
            Some(permits) => Some(acquire(permits, &worker_ctx).await?),
            None => None,
        };
        if worker_ctx.is_cancelled() {
            return Err(Fail::Cancelled);
        }
        let blocking_ctx = Arc::clone(&worker_ctx);
        match tokio::task::spawn_blocking(move || run(work, &settings, &blocking_ctx)).await {
            Ok(result) => result,
            Err(err) if err.is_panic() => {
                tracing::error!(job = id, "job panicked");
                Err(Fail::Failed("internal error: the job panicked".to_owned()))
            }
            Err(err) => Err(Fail::Failed(format!("the job was aborted: {err}"))),
        }
    };
    tokio::pin!(task);
    let mut tick = tokio::time::interval(SAMPLE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last: Option<Progress> = None;
    let result = loop {
        tokio::select! {
            result = &mut task => break result,
            _ = tick.tick() => {
                let progress = ctx.snapshot();
                if last.as_ref() != Some(&progress) {
                    last = Some(progress.clone());
                    notify(&engine, id, kind, JobState::Running, progress);
                }
            }
        }
    };
    let (state, output) = match result {
        Ok(output) if ctx.is_cancelled() => (JobState::Cancelled, Some(output)),
        Ok(output) => (JobState::Done, Some(output)),
        Err(Fail::Cancelled) => (JobState::Cancelled, None),
        Err(Fail::Failed(message)) => (JobState::Failed { message }, None),
    };
    tracing::info!(job = id, ?kind, ?state, "job finished");
    engine.jobs().finish(id, state.clone(), output);
    notify(&engine, id, kind, state, ctx.snapshot());
}

fn notify(engine: &Engine, job: JobId, kind: JobKind, state: JobState, progress: Progress) {
    engine.notify_ui(Event::Job(JobUpdate {
        job,
        kind,
        status: JobStatus { state, progress },
    }));
}

/// Waits (in FIFO order) for the mutation permit, giving up when the job is cancelled.
async fn acquire(permits: Arc<Semaphore>, ctx: &JobCtx) -> Result<OwnedSemaphorePermit, Fail> {
    let permit = permits.acquire_owned();
    tokio::pin!(permit);
    loop {
        if ctx.is_cancelled() {
            return Err(Fail::Cancelled);
        }
        tokio::select! {
            permit = &mut permit => {
                return permit.map_err(|_closed| Fail::Failed("job queue closed".to_owned()));
            }
            () = tokio::time::sleep(SAMPLE_EVERY) => {}
        }
    }
}

/// A walker for this job; junk-style scans always include hidden entries.
fn walker(settings: &CleanSettings, skip_hidden: bool) -> Result<Walker, Fail> {
    let mut opts = WalkOptions::from_settings(settings);
    opts.skip_hidden = skip_hidden;
    Ok(Walker::new(opts)?)
}

/// Roots of the file scans: the configured folders, or the home folder.
fn file_roots(settings: &CleanSettings) -> Result<Vec<PathBuf>, Fail> {
    if settings.file_roots.is_empty() {
        let home = omc_scan::paths::home()
            .ok_or_else(|| Fail::Failed("the home folder is unknown".to_owned()))?;
        return Ok(vec![home]);
    }
    Ok(settings
        .file_roots
        .iter()
        .map(|root| omc_scan::paths::expand(root))
        .collect())
}

/// The job body (blocking thread).
fn run(work: Work, s: &CleanSettings, ctx: &JobCtx) -> Result<Retained, Fail> {
    Ok(match work {
        Work::Junk(area) => {
            let walker = walker(s, false)?;
            let mut scan = omc_scan::junk::scan(area, s, &walker, ctx)?;
            // Only system junk is named by bundle ids and app folders; browser, developer,
            // trash and installer items already carry readable names.
            if area == JunkArea::System {
                omc_apps::annotate_junk(&mut scan.report, s, &walker, ctx);
            }
            Retained::Junk(scan)
        }
        Work::Leftovers => Retained::Junk(omc_apps::leftovers(s, &walker(s, false)?, ctx)?),
        Work::Large => {
            let roots = file_roots(s)?;
            Retained::Files(omc_scan::large::scan(
                &roots,
                s,
                &walker(s, s.skip_hidden)?,
                ctx,
            )?)
        }
        Work::Dupes => {
            let roots = file_roots(s)?;
            Retained::Dupes(omc_scan::dupes::scan(
                &roots,
                s,
                &walker(s, s.skip_hidden)?,
                ctx,
            )?)
        }
        Work::Space(root) => Retained::Space(omc_scan::space::scan(
            &root,
            &walker(s, s.skip_hidden)?,
            ctx,
        )?),
        Work::Clean { source, items } => {
            let targets = match &*source {
                Retained::Junk(scan) => scan.select(&items),
                Retained::Files(scan) => scan.select(&items),
                Retained::Dupes(scan) => scan.select(&items),
                Retained::Space(tree) => items
                    .iter()
                    .filter_map(|node| tree.target(*node, s.files_delete))
                    .collect(),
                _ => return Err(Fail::Failed("the source job is not a scan".to_owned())),
            };
            Retained::Clean(omc_apps::remove(&targets, s, ctx))
        }
        Work::ListApps => Retained::Apps(omc_apps::list_apps(s, &walker(s, false)?, ctx)?),
        Work::AppFiles(app) => {
            Retained::AppFiles(omc_apps::app_files(&app, s, &walker(s, false)?, ctx)?)
        }
        Work::Uninstall {
            source,
            items,
            run_uninstaller,
        } => {
            let Retained::AppFiles(files) = &*source else {
                return Err(Fail::Failed(
                    "the source job is not an app_files job".to_owned(),
                ));
            };
            Retained::Uninstall(omc_apps::uninstall(files, &items, run_uninstaller, s, ctx)?)
        }
        Work::ListStartup => Retained::Startup(omc_apps::list_startup(s, ctx)?),
        Work::ChangeStartup { record, change } => {
            Retained::StartupChanged(omc_apps::change_startup(&record, change, s, ctx)?)
        }
    })
}
