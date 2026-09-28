//! Job tracking shared by the app pages: one [`JobSlot`] per job a page follows, driven
//! by `start_job` → job updates / `job_status` → `job_result`, with stale results dropped
//! by revision and job id.

use gpui_kit::{AsyncApp, Context, SharedString, Task, WeakEntity};
use omc_proto::jobs::{JobId, JobOutput, JobSpec, JobState, JobStatus, JobUpdate, Progress};

use crate::jobs;
use crate::pages::widgets::release;
use crate::tokens::page;

/// One job a page follows.
#[derive(Default)]
pub(crate) struct JobSlot {
    /// The job; kept after it finished until the page takes or resets it.
    pub(crate) job: Option<JobId>,
    /// Started and not yet delivered or failed.
    active: bool,
    /// Its output is being fetched.
    finishing: bool,
    /// Bumped whenever in-flight results become stale.
    rev: u64,
    /// Latest progress.
    pub(crate) progress: Progress,
    task: Option<Task<()>>,
}

impl std::fmt::Debug for JobSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobSlot")
            .field("job", &self.job)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl JobSlot {
    /// Started and not finished.
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    /// Stops following the job; returns its id (the caller decides whether to
    /// release it: a job of an old daemon epoch is simply forgotten).
    pub(crate) fn reset(&mut self) -> Option<JobId> {
        self.rev = self.rev.wrapping_add(1);
        self.active = false;
        self.finishing = false;
        self.progress = Progress::default();
        self.task = None;
        self.job.take()
    }
}

/// A view that follows jobs in slots.
pub(crate) trait SlotHost: Sized + 'static {
    /// Slot identifier.
    type Slot: Copy + 'static;
    /// The slot's state.
    fn slot(&mut self, slot: Self::Slot) -> Option<&mut JobSlot>;
    /// A job finished (or was cancelled) with `output`.
    fn output(
        &mut self,
        slot: Self::Slot,
        job: JobId,
        output: JobOutput,
        cx: &mut Context<'_, Self>,
    );
    /// A job could not start, failed, or its output could not be fetched.
    fn failed(&mut self, slot: Self::Slot, message: SharedString, cx: &mut Context<'_, Self>);
    /// Pending coalesced re-render.
    fn notify_task(&mut self) -> &mut Option<Task<()>>;
}

fn gone<T>(result: Result<T, impl std::fmt::Display>) {
    if let Err(err) = result {
        tracing::debug!("page closed before a job answered: {err}");
    }
}

/// Starts `spec` in `slot`, releasing the slot's previous job.
pub(crate) fn start<H: SlotHost>(
    host: &mut H,
    slot: H::Slot,
    spec: JobSpec,
    cx: &mut Context<'_, H>,
) {
    let Some(s) = host.slot(slot) else { return };
    if let Some(old) = s.reset() {
        release(old, cx);
    }
    s.active = true;
    let rev = s.rev;
    let start = jobs::start_job(spec, cx);
    s.task = Some(
        cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
            let result = start.await;
            gone(this.update(cx, |host, cx| started(host, slot, rev, result, cx)));
        }),
    );
}

fn started<H: SlotHost>(
    host: &mut H,
    slot: H::Slot,
    rev: u64,
    result: Result<JobId, jobs::Failure>,
    cx: &mut Context<'_, H>,
) {
    let current = host.slot(slot).is_some_and(|s| s.rev == rev && s.active);
    match result {
        Ok(id) if !current => release(id, cx),
        Ok(id) => {
            if let Some(s) = host.slot(slot) {
                s.job = Some(id);
            }
            poll(host, slot, cx);
        }
        Err(_) if !current => {}
        Err(err) => {
            if let Some(s) = host.slot(slot) {
                s.active = false;
            }
            host.failed(slot, err.into(), cx);
        }
    }
}

/// Asks the daemon for the slot's job status (after starting it, or after a
/// reconnect that may have missed its updates).
pub(crate) fn poll<H: SlotHost>(host: &mut H, slot: H::Slot, cx: &mut Context<'_, H>) {
    let Some(s) = host.slot(slot) else { return };
    let Some(id) = s.job.filter(|_| s.active && !s.finishing) else {
        return;
    };
    let rev = s.rev;
    let status = jobs::job_status(id, cx);
    s.task = Some(
        cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
            let result = status.await;
            gone(this.update(cx, |host, cx| match result {
                Ok(status) => on_status(host, slot, rev, id, &status, cx),
                Err(err) => fail(host, slot, rev, err.into(), cx),
            }));
        }),
    );
}

/// Routes a pushed job update to the slot following that job.
pub(crate) fn route<H: SlotHost>(
    host: &mut H,
    slots: &[H::Slot],
    update: &JobUpdate,
    cx: &mut Context<'_, H>,
) {
    for &slot in slots {
        let rev = match host.slot(slot) {
            Some(s) if s.job == Some(update.job) => s.rev,
            _ => continue,
        };
        on_status(host, slot, rev, update.job, &update.status, cx);
    }
}

fn on_status<H: SlotHost>(
    host: &mut H,
    slot: H::Slot,
    rev: u64,
    id: JobId,
    status: &JobStatus,
    cx: &mut Context<'_, H>,
) {
    let Some(s) = host.slot(slot) else { return };
    if s.rev != rev || s.job != Some(id) || !s.active || s.finishing {
        return;
    }
    s.progress = status.progress.clone();
    match &status.state {
        JobState::Running => schedule_notify(host, cx),
        JobState::Done | JobState::Cancelled => {
            s.finishing = true;
            let result = jobs::job_result(id, cx);
            s.task = Some(
                cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
                    let result = result.await;
                    gone(this.update(cx, |host, cx| delivered(host, slot, rev, id, result, cx)));
                }),
            );
            cx.notify();
        }
        JobState::Failed { message } => {
            s.active = false;
            host.failed(slot, message.clone().into(), cx);
        }
    }
}

fn delivered<H: SlotHost>(
    host: &mut H,
    slot: H::Slot,
    rev: u64,
    id: JobId,
    result: Result<JobOutput, jobs::Failure>,
    cx: &mut Context<'_, H>,
) {
    let Some(s) = host.slot(slot) else { return };
    if s.rev != rev || s.job != Some(id) || !s.active {
        return;
    }
    s.active = false;
    s.finishing = false;
    match result {
        Ok(output) => host.output(slot, id, output, cx),
        Err(err) => host.failed(slot, err.into(), cx),
    }
}

fn fail<H: SlotHost>(
    host: &mut H,
    slot: H::Slot,
    rev: u64,
    message: SharedString,
    cx: &mut Context<'_, H>,
) {
    let Some(s) = host.slot(slot) else { return };
    if s.rev != rev || !s.active {
        return;
    }
    s.active = false;
    host.failed(slot, message, cx);
}

/// Asks the slot's running job to stop.
pub(crate) fn cancel<H: SlotHost>(host: &mut H, slot: H::Slot, cx: &mut Context<'_, H>) {
    let Some(id) = host.slot(slot).and_then(|s| s.job.filter(|_| s.active)) else {
        return;
    };
    let cancel = jobs::cancel_job(id, cx);
    cx.spawn(async move |_: WeakEntity<H>, _: &mut AsyncApp| {
        if let Err(err) = cancel.await {
            tracing::warn!(job = id, "cancel failed: {err}");
        }
    })
    .detach();
}

/// Re-renders at most once per [`page::STREAM_COALESCE`].
pub(crate) fn schedule_notify<H: SlotHost>(host: &mut H, cx: &mut Context<'_, H>) {
    let pending = host.notify_task();
    if pending.is_some() {
        return;
    }
    *pending = Some(
        cx.spawn(async move |this: WeakEntity<H>, cx: &mut AsyncApp| {
            cx.background_executor().timer(page::STREAM_COALESCE).await;
            gone(this.update(cx, |host, cx| {
                *host.notify_task() = None;
                cx.notify();
            }));
        }),
    );
}
