//! The scan → review → clean lifecycle of one scan area, driven from the daemon's job
//! updates (`crate::jobs` + the engine's `ClientEvent`s).
//!
//! The shared store ([`crate::scans::Scans`]) embeds one [`Flow`] per scan area (keyed by
//! the area's slot) and implements [`FlowHost`] so the flow can hand over finished
//! outputs. All operations are associated functions taking the host, so async results can
//! find their flow again after an `await`; each result is checked against the flow's
//! revision and job id, so a result that arrives after a rescan, cancel or reconnect is
//! dropped. Views render a [`FlowView`] copied out of the store.

use gpui_kit::{App, AsyncApp, Context, SharedString, Task, WeakEntity};
use omc_ipc::client::{ClientEvent, ConnState};
use omc_proto::Event;
use omc_proto::jobs::{
    CleanReport, CleanSpec, ItemId, JobId, JobOutput, JobSpec, JobState, JobStatus, Progress,
};

use crate::engine;
use crate::jobs;
use crate::tokens::page;

/// Where a flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FlowPhase {
    /// Nothing scanned yet (or the results were dropped).
    #[default]
    Idle,
    /// A scan job is starting, running, or its output is being fetched.
    Scanning,
    /// The scan output is shown and can be cleaned.
    Ready,
    /// A clean job is running.
    Cleaning,
    /// A clean finished; its report is shown.
    Cleaned,
    /// The scan could not run; [`Flow::error`] says why.
    Failed,
}

/// One scan and the cleans of its items.
#[derive(Default)]
pub(crate) struct Flow {
    phase: FlowPhase,
    scan_job: Option<JobId>,
    clean_job: Option<JobId>,
    /// Items sent to the running (or last) clean.
    cleaning: Vec<ItemId>,
    progress: Progress,
    report: Option<CleanReport>,
    error: Option<SharedString>,
    /// The running scan was asked to stop.
    cancelled: bool,
    /// Bumped by every operation that makes in-flight async results stale.
    rev: u64,
    task: Option<Task<()>>,
}

impl std::fmt::Debug for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Flow")
            .field("phase", &self.phase)
            .field("scan_job", &self.scan_job)
            .field("clean_job", &self.clean_job)
            .finish_non_exhaustive()
    }
}

/// What a view renders of a [`Flow`], copied out of the store (views cannot keep a borrow
/// of the store while they build listeners).
#[derive(Debug, Clone, Default)]
pub(crate) struct FlowView {
    /// Current phase.
    pub(crate) phase: FlowPhase,
    /// Latest progress of the running job.
    pub(crate) progress: Progress,
    /// Why the scan or the last clean failed.
    pub(crate) error: Option<SharedString>,
    /// Report of the last clean (shown in [`FlowPhase::Cleaned`]).
    pub(crate) report: Option<CleanReport>,
}

impl FlowView {
    /// A job is starting or running.
    pub(crate) fn is_busy(&self) -> bool {
        matches!(self.phase, FlowPhase::Scanning | FlowPhase::Cleaning)
    }
}

/// A view that runs [`Flow`]s.
pub(crate) trait FlowHost: Sized + 'static {
    /// The flow with `key` (single-flow pages ignore the key).
    fn flow(&mut self, key: usize) -> Option<&mut Flow>;
    /// Coalesces progress re-renders.
    fn throttle(&mut self) -> &mut Throttle;
    /// A scan finished (or was cancelled with a partial output); build the results.
    fn scanned(&mut self, key: usize, output: JobOutput, cx: &mut Context<'_, Self>);
    /// The results of `key` are gone (rescan started, daemon restarted); drop them.
    fn cleared(&mut self, key: usize, cx: &mut Context<'_, Self>);
    /// A clean of `items` finished with `report`; drop what was removed.
    fn cleaned(
        &mut self,
        key: usize,
        items: &[ItemId],
        report: &CleanReport,
        cx: &mut Context<'_, Self>,
    );
}

impl Flow {
    /// Current phase.
    pub(crate) fn phase(&self) -> FlowPhase {
        self.phase
    }

    /// The finished scan job whose items can be cleaned.
    pub(crate) fn scan_job(&self) -> Option<JobId> {
        self.scan_job
    }

    /// A job is starting or running.
    pub(crate) fn is_busy(&self) -> bool {
        matches!(self.phase, FlowPhase::Scanning | FlowPhase::Cleaning)
    }

    /// A copy of what views render.
    pub(crate) fn view(&self) -> FlowView {
        FlowView {
            phase: self.phase,
            progress: self.progress.clone(),
            error: self.error.clone(),
            report: self.report.clone(),
        }
    }

    /// Puts the flow in the results state without a daemon (render tests).
    #[cfg(test)]
    pub(crate) fn force_ready(&mut self, scan_job: Option<JobId>) {
        self.phase = FlowPhase::Ready;
        self.scan_job = scan_job;
    }

    /// Starts a scan, releasing the previous one and dropping its results.
    pub(crate) fn scan<H: FlowHost>(
        host: &mut H,
        key: usize,
        spec: JobSpec,
        cx: &mut Context<'_, H>,
    ) {
        let old = host.flow(key).map(|flow| {
            let old = [flow.scan_job.take(), flow.clean_job.take()];
            flow.rev = flow.rev.wrapping_add(1);
            flow.phase = FlowPhase::Scanning;
            flow.progress = Progress::default();
            flow.report = None;
            flow.error = None;
            flow.cancelled = false;
            flow.cleaning.clear();
            old
        });
        let Some(old) = old else { return };
        for job in old.into_iter().flatten() {
            release(job, cx);
        }
        host.cleared(key, cx);
        let Some(rev) = host.flow(key).map(|flow| flow.rev) else {
            return;
        };
        let start = jobs::start_job(spec, cx);
        let task = cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
            let started = start.await;
            let applied = this.update(cx, |host, cx| {
                let Some(flow) = host.flow(key).filter(|flow| flow.rev == rev) else {
                    // Superseded while starting: free the orphan.
                    if let Ok(job) = started {
                        release(job, cx);
                    }
                    return;
                };
                match started {
                    Ok(job) => flow.scan_job = Some(job),
                    Err(message) => {
                        flow.phase = FlowPhase::Failed;
                        flow.error = Some(message.into());
                    }
                }
                cx.notify();
            });
            log_gone(applied);
        });
        if let Some(flow) = host.flow(key) {
            flow.task = Some(task);
        }
        cx.notify();
    }

    /// Stops the running scan or clean. A scan that has not started yet goes back to idle;
    /// a running job is asked to stop and its (partial) output is shown when it ends.
    pub(crate) fn cancel<H: FlowHost>(host: &mut H, key: usize, cx: &mut Context<'_, H>) {
        let Some(flow) = host.flow(key) else { return };
        let job = match flow.phase {
            FlowPhase::Scanning => flow.scan_job,
            FlowPhase::Cleaning => flow.clean_job,
            _ => return,
        };
        let Some(job) = job else {
            flow.rev = flow.rev.wrapping_add(1);
            flow.phase = if flow.phase == FlowPhase::Cleaning {
                FlowPhase::Ready
            } else {
                FlowPhase::Idle
            };
            flow.task = None;
            cx.notify();
            return;
        };
        flow.cancelled = true;
        let cancel = jobs::cancel_job(job, cx);
        cx.spawn(async move |_, _| {
            if let Err(err) = cancel.await {
                tracing::warn!(job, "cancel failed: {err}");
            }
        })
        .detach();
    }

    /// Removes `items` of the finished scan.
    pub(crate) fn clean<H: FlowHost>(
        host: &mut H,
        key: usize,
        items: Vec<ItemId>,
        cx: &mut Context<'_, H>,
    ) {
        let Some(flow) = host.flow(key) else { return };
        let Some(scan_job) = flow.scan_job else {
            return;
        };
        if items.is_empty() || flow.phase != FlowPhase::Ready {
            return;
        }
        let old_clean = flow.clean_job.take();
        flow.rev = flow.rev.wrapping_add(1);
        flow.phase = FlowPhase::Cleaning;
        flow.progress = Progress::default();
        flow.report = None;
        flow.error = None;
        flow.cancelled = false;
        flow.cleaning.clone_from(&items);
        let rev = flow.rev;
        if let Some(job) = old_clean {
            release(job, cx);
        }
        let start = jobs::start_job(JobSpec::Clean(CleanSpec { scan_job, items }), cx);
        let task = cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
            let started = start.await;
            let applied = this.update(cx, |host, cx| {
                let Some(flow) = host.flow(key).filter(|flow| flow.rev == rev) else {
                    if let Ok(job) = started {
                        release(job, cx);
                    }
                    return;
                };
                match started {
                    Ok(job) => flow.clean_job = Some(job),
                    Err(message) => {
                        flow.phase = FlowPhase::Ready;
                        flow.error = Some(message.into());
                    }
                }
                cx.notify();
            });
            log_gone(applied);
        });
        if let Some(flow) = host.flow(key) {
            flow.task = Some(task);
        }
        cx.notify();
    }

    /// Leaves the clean report and shows the remaining results.
    pub(crate) fn dismiss_report<H: FlowHost>(host: &mut H, key: usize, cx: &mut Context<'_, H>) {
        if let Some(flow) = host.flow(key)
            && flow.phase == FlowPhase::Cleaned
        {
            flow.phase = FlowPhase::Ready;
            cx.notify();
        }
    }

    /// Clears the error banner.
    pub(crate) fn dismiss_error<H: FlowHost>(host: &mut H, key: usize, cx: &mut Context<'_, H>) {
        if let Some(flow) = host.flow(key) {
            flow.error = None;
            if flow.phase == FlowPhase::Failed {
                flow.phase = FlowPhase::Idle;
            }
            cx.notify();
        }
    }

    /// Routes an engine event to flow `key`. `change` is the page's reading of the
    /// connection (see [`Connection::observe`]); pass it for every flow of the page.
    pub(crate) fn on_event<H: FlowHost>(
        host: &mut H,
        key: usize,
        event: &ClientEvent,
        change: ConnChange,
        cx: &mut Context<'_, H>,
    ) {
        match (event, change) {
            (_, ConnChange::NewDaemon) => Self::stale(host, key, cx),
            (_, ConnChange::Reattached) => Self::poll(host, key, cx),
            (ClientEvent::Daemon(Event::Job(update)), ConnChange::None) => {
                Self::apply(host, key, update.job, &update.status, cx);
            }
            _ => {}
        }
    }

    /// The daemon restarted: every job id is meaningless now.
    fn stale<H: FlowHost>(host: &mut H, key: usize, cx: &mut Context<'_, H>) {
        let Some(flow) = host.flow(key) else { return };
        if flow.phase == FlowPhase::Idle && flow.scan_job.is_none() {
            return;
        }
        let was_busy = flow.is_busy();
        *flow = Self {
            rev: flow.rev.wrapping_add(1),
            error: was_busy.then(|| tr("scan.restarted")),
            phase: if was_busy {
                FlowPhase::Failed
            } else {
                FlowPhase::Idle
            },
            ..Self::default()
        };
        host.cleared(key, cx);
        cx.notify();
    }

    /// Same daemon, new connection: job updates may have been lost, so ask for the
    /// status of the running job.
    fn poll<H: FlowHost>(host: &mut H, key: usize, cx: &mut Context<'_, H>) {
        let Some(flow) = host.flow(key) else { return };
        let job = match flow.phase {
            FlowPhase::Scanning => flow.scan_job,
            FlowPhase::Cleaning => flow.clean_job,
            _ => None,
        };
        let Some(job) = job else { return };
        let status = jobs::job_status(job, cx);
        let task =
            cx.spawn(
                async move |this: WeakEntity<H>, cx: &mut AsyncApp| match status.await {
                    Ok(status) => {
                        log_gone(
                            this.update(cx, |host, cx| Self::apply(host, key, job, &status, cx)),
                        );
                    }
                    Err(err) => tracing::debug!(job, "job status after reconnect: {err}"),
                },
            );
        if let Some(flow) = host.flow(key) {
            flow.task = Some(task);
        }
    }

    /// A status of `job` arrived (pushed update or polled).
    fn apply<H: FlowHost>(
        host: &mut H,
        key: usize,
        job: JobId,
        status: &JobStatus,
        cx: &mut Context<'_, H>,
    ) {
        let Some(flow) = host.flow(key) else { return };
        let scanning = flow.phase == FlowPhase::Scanning && flow.scan_job == Some(job);
        let cleaning = flow.phase == FlowPhase::Cleaning && flow.clean_job == Some(job);
        if !scanning && !cleaning {
            return;
        }
        flow.progress.clone_from(&status.progress);
        match &status.state {
            JobState::Running => {
                Throttle::schedule(host, cx);
                return;
            }
            JobState::Failed { message } => {
                flow.rev = flow.rev.wrapping_add(1);
                flow.error = Some(message.clone().into());
                if scanning {
                    flow.phase = FlowPhase::Failed;
                    flow.scan_job = None;
                } else {
                    flow.phase = FlowPhase::Ready;
                    flow.clean_job = None;
                }
                release(job, cx);
            }
            JobState::Done | JobState::Cancelled => {
                flow.rev = flow.rev.wrapping_add(1);
                let rev = flow.rev;
                let result = jobs::job_result(job, cx);
                let task = cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
                    let output = result.await;
                    log_gone(this.update(cx, |host, cx| {
                        Self::finished(host, key, rev, job, output, cx);
                    }));
                });
                if let Some(flow) = host.flow(key) {
                    flow.task = Some(task);
                }
            }
        }
        cx.notify();
    }

    fn finished<H: FlowHost>(
        host: &mut H,
        key: usize,
        rev: u64,
        job: JobId,
        output: Result<JobOutput, jobs::Failure>,
        cx: &mut Context<'_, H>,
    ) {
        let Some(flow) = host.flow(key).filter(|flow| flow.rev == rev) else {
            return;
        };
        match (flow.phase, output) {
            (FlowPhase::Scanning, Ok(output)) => {
                flow.phase = FlowPhase::Ready;
                host.scanned(key, output, cx);
            }
            (FlowPhase::Cleaning, Ok(JobOutput::Clean(report))) => {
                flow.phase = FlowPhase::Cleaned;
                flow.clean_job = None;
                let items = std::mem::take(&mut flow.cleaning);
                release(job, cx);
                host.cleaned(key, &items, &report, cx);
                if let Some(flow) = host.flow(key) {
                    flow.report = Some(report);
                }
            }
            (FlowPhase::Cleaning, Ok(other)) => {
                tracing::warn!(?other, "a clean job answered with another output");
                flow.phase = FlowPhase::Ready;
                flow.clean_job = None;
                release(job, cx);
            }
            (FlowPhase::Scanning, Err(message)) => {
                // A cancelled scan without output goes back to idle; anything else failed.
                flow.scan_job = None;
                if flow.cancelled {
                    flow.phase = FlowPhase::Idle;
                } else {
                    flow.phase = FlowPhase::Failed;
                    flow.error = Some(message.into());
                }
                release(job, cx);
                host.cleared(key, cx);
            }
            (FlowPhase::Cleaning, Err(message)) => {
                flow.phase = FlowPhase::Ready;
                flow.clean_job = None;
                flow.error = Some(message.into());
                release(job, cx);
            }
            _ => {}
        }
        cx.notify();
    }
}

/// Frees `job`'s output in the daemon, in the background.
pub(crate) fn release(job: JobId, cx: &mut App) {
    let release = jobs::release_job(job, cx);
    cx.spawn(async move |_| {
        if let Err(err) = release.await {
            tracing::debug!(job, "release failed: {err}");
        }
    })
    .detach();
}

fn log_gone<T>(result: Result<T, impl std::fmt::Display>) {
    if let Err(err) = result {
        tracing::debug!("page closed before a job answered: {err}");
    }
}

/// Coalesces streamed re-renders to at most one per [`page::STREAM_COALESCE`].
#[derive(Default)]
pub(crate) struct Throttle {
    pending: Option<Task<()>>,
}

impl std::fmt::Debug for Throttle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Throttle")
            .field("pending", &self.pending.is_some())
            .finish()
    }
}

impl Throttle {
    /// Re-renders `host` once the coalescing window ends (no-op while one is pending).
    pub(crate) fn schedule<H: FlowHost>(host: &mut H, cx: &mut Context<'_, H>) {
        if host.throttle().pending.is_some() {
            return;
        }
        let task = cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
            cx.background_executor().timer(page::STREAM_COALESCE).await;
            log_gone(this.update(cx, |host, cx| {
                host.throttle().pending = None;
                cx.notify();
            }));
        });
        host.throttle().pending = Some(task);
    }
}

/// How a connection-state change affects the jobs of a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnChange {
    /// Nothing that affects jobs.
    None,
    /// Connected to a daemon with another epoch: every job id is stale.
    NewDaemon,
    /// Connected again to the same daemon: updates may have been missed.
    Reattached,
}

/// Tracks the daemon epoch a page's job ids belong to.
#[derive(Debug, Default)]
pub(crate) struct Connection {
    epoch: Option<String>,
    connected: bool,
}

impl Connection {
    /// Starts from the engine's current state.
    pub(crate) fn new(cx: &mut App) -> Self {
        let mut this = Self::default();
        let state = engine::entity(cx).read(cx).state().clone();
        this.observe_state(&state);
        this
    }

    /// Whether requests can be sent now.
    pub(crate) fn is_connected(&self) -> bool {
        self.connected
    }

    /// Reads an engine event.
    pub(crate) fn observe(&mut self, event: &ClientEvent) -> ConnChange {
        if let ClientEvent::State(state) = event {
            self.observe_state(state)
        } else {
            ConnChange::None
        }
    }

    fn observe_state(&mut self, state: &ConnState) -> ConnChange {
        let ConnState::Connected { epoch } = state else {
            self.connected = false;
            return ConnChange::None;
        };
        let was_connected = std::mem::replace(&mut self.connected, true);
        match self.epoch.replace(epoch.clone()) {
            Some(old) if old != *epoch => ConnChange::NewDaemon,
            Some(_) if !was_connected => ConnChange::Reattached,
            _ => ConnChange::None,
        }
    }
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}

#[cfg(test)]
mod tests {
    use omc_ipc::client::{ClientEvent, ConnState};

    use super::{ConnChange, Connection};

    fn connected(epoch: &str) -> ClientEvent {
        ClientEvent::State(ConnState::Connected {
            epoch: epoch.to_owned(),
        })
    }

    #[test]
    fn only_a_new_epoch_makes_jobs_stale() {
        let mut conn = Connection::default();
        assert_eq!(
            conn.observe(&connected("a")),
            ConnChange::None,
            "first connect"
        );
        assert!(conn.is_connected(), "connected after the first epoch");
        assert_eq!(conn.observe(&connected("a")), ConnChange::None, "repeat");
        let lost = ClientEvent::State(ConnState::Reconnecting {
            attempt: 1,
            reason: "gone".to_owned(),
        });
        assert_eq!(
            conn.observe(&lost),
            ConnChange::None,
            "losing the connection"
        );
        assert!(!conn.is_connected(), "disconnected while reconnecting");
        assert_eq!(
            conn.observe(&connected("a")),
            ConnChange::Reattached,
            "same daemon again: poll for missed updates"
        );
        assert_eq!(
            conn.observe(&connected("b")),
            ConnChange::NewDaemon,
            "another epoch: drop every job id"
        );
    }
}
