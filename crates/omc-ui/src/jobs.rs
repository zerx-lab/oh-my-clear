//! Typed daemon requests for views. Each helper grabs the connection handle synchronously
//! and returns a `'static` future to await inside `cx.spawn` (tokio channel futures run on
//! GPUI's executor). Errors come back as display text: views show them, nothing retries
//! behind the user's back.
//!
//! Job lifecycle from a view: [`start_job`] → follow `Event::Job` updates for that id from
//! the [`crate::engine::Engine`] entity (subscribe to its `ClientEvent`s) → once the state
//! is finished, [`job_result`] → keep the output while shown → [`release_job`] when replaced.
//! After a reconnect with a new epoch every job id is stale: drop it and show the idle state.

use gpui_kit::App;
use omc_proto::files::SpaceListing;
use omc_proto::jobs::{ItemId, JobId, JobOutput, JobSpec, JobStatus};
use omc_proto::settings::{Settings, SystemInfo};
use omc_proto::{Request, Response};

use crate::engine;

/// Error text shown to the user.
pub type Failure = String;

/// Sends `req`; resolves to the daemon's answer.
pub fn request(
    req: Request,
    cx: &mut App,
) -> impl Future<Output = Result<Response, Failure>> + use<> {
    let handle = engine::entity(cx).read(cx).handle().cloned();
    async move {
        let Some(handle) = handle else {
            return Err(rust_i18n::t!("status.unavailable").to_string());
        };
        handle.request(req).await.map_err(|e| e.to_string())
    }
}

fn unexpected(what: &str, got: &Response) -> Failure {
    tracing::warn!(?got, "unexpected answer to {what}");
    format!("unexpected answer to {what}")
}

/// Starts a job.
pub fn start_job(
    spec: JobSpec,
    cx: &mut App,
) -> impl Future<Output = Result<JobId, Failure>> + use<> {
    let answer = request(Request::StartJob(spec), cx);
    async move {
        match answer.await? {
            Response::Job { job } => Ok(job),
            other => Err(unexpected("start_job", &other)),
        }
    }
}

/// A job's status (used after a reconnect or a lagged update).
pub fn job_status(
    job: JobId,
    cx: &mut App,
) -> impl Future<Output = Result<JobStatus, Failure>> + use<> {
    let answer = request(Request::JobStatus { job }, cx);
    async move {
        match answer.await? {
            Response::JobStatus(status) => Ok(status),
            other => Err(unexpected("job_status", &other)),
        }
    }
}

/// A finished job's output.
pub fn job_result(
    job: JobId,
    cx: &mut App,
) -> impl Future<Output = Result<JobOutput, Failure>> + use<> {
    let answer = request(Request::JobResult { job }, cx);
    async move {
        match answer.await? {
            Response::JobResult(output) => Ok(output),
            other => Err(unexpected("job_result", &other)),
        }
    }
}

/// Asks a running job to stop.
pub fn cancel_job(job: JobId, cx: &mut App) -> impl Future<Output = Result<(), Failure>> + use<> {
    unit(request(Request::CancelJob { job }, cx), "cancel_job")
}

/// Frees a finished job's output in the daemon.
pub fn release_job(job: JobId, cx: &mut App) -> impl Future<Output = Result<(), Failure>> + use<> {
    unit(request(Request::ReleaseJob { job }, cx), "release_job")
}

/// Children of a directory node of a finished space-lens job.
pub fn space_children(
    job: JobId,
    node: ItemId,
    cx: &mut App,
) -> impl Future<Output = Result<SpaceListing, Failure>> + use<> {
    let answer = request(Request::SpaceChildren { job, node }, cx);
    async move {
        match answer.await? {
            Response::SpaceNodes(listing) => Ok(listing),
            other => Err(unexpected("space_children", &other)),
        }
    }
}

/// OS, permissions and volumes.
pub fn system_info(cx: &mut App) -> impl Future<Output = Result<SystemInfo, Failure>> + use<> {
    let answer = request(Request::SystemInfo, cx);
    async move {
        match answer.await? {
            Response::SystemInfo(info) => Ok(info),
            other => Err(unexpected("system_info", &other)),
        }
    }
}

/// The daemon's stored settings.
pub fn get_settings(cx: &mut App) -> impl Future<Output = Result<Settings, Failure>> + use<> {
    let answer = request(Request::GetSettings, cx);
    async move {
        match answer.await? {
            Response::Settings(settings) => Ok(settings),
            other => Err(unexpected("get_settings", &other)),
        }
    }
}

/// Replaces and persists the daemon's settings.
pub fn put_settings(
    settings: Settings,
    cx: &mut App,
) -> impl Future<Output = Result<(), Failure>> + use<> {
    unit(request(Request::PutSettings(settings), cx), "put_settings")
}

async fn unit(
    answer: impl Future<Output = Result<Response, Failure>>,
    what: &'static str,
) -> Result<(), Failure> {
    match answer.await? {
        Response::Unit => Ok(()),
        other => Err(unexpected(what, &other)),
    }
}
