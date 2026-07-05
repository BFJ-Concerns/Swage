//! Client surface: types spoken between the gateway and its own clients
//! (web frontend, TUI, desktop shell) over HTTP and WebSocket.

pub mod channel;
pub mod delivery;
pub mod schedule;

use schemars::JsonSchema;

/// Anchor type for the client surface's schema document.
///
/// Every wire type on the surface is reachable from here, so a single
/// `schema_for!` call produces one document with the full type graph in
/// `$defs`. The struct itself never crosses the wire — it exists only so
/// the schema generator has a root to walk from.
#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct ClientSurface {
    create_channel_link_code_request: channel::CreateChannelLinkCodeRequest,
    channel_link_code_response: channel::ChannelLinkCodeResponse,
    create_schedule_request: schedule::CreateScheduleRequest,
    update_schedule_request: schedule::UpdateScheduleRequest,
    schedule: schedule::Schedule,
    schedule_list: schedule::ScheduleListResponse,
    schedule_run: schedule::ScheduleRun,
    schedule_run_list: schedule::ScheduleRunListResponse,
    trigger_schedule_response: schedule::TriggerScheduleResponse,
}
