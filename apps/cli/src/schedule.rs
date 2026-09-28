//! Scheduled tasks (ADR 0063).
//!
//! Schedules run while the desktop application is open. From a terminal,
//! `run-due` checks once, which is also how a person can drive them from a
//! scheduler of their own.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand, ValueEnum};
use tramontana_core::contracts::{Cadence, NewSchedule, ProjectId, RoutingMode, Weekday};

use crate::run::ModeArg;
use crate::{render, wiring};

/// Arguments for `tramontana schedule`.
#[derive(Debug, Args)]
pub(crate) struct ScheduleArgs {
    #[command(subcommand)]
    command: ScheduleCommand,

    /// Database file to use.
    #[arg(long, value_name = "PATH", global = true)]
    profile: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum ScheduleCommand {
    /// List schedules and when each runs next, as JSON.
    List,
    /// Create a schedule and the conversation its runs are turns in.
    Add(AddArgs),
    /// Turn a schedule on.
    Enable { schedule_id: String },
    /// Turn a schedule off; it and its history are kept.
    Disable { schedule_id: String },
    /// Delete a schedule; its conversation is kept.
    Remove { schedule_id: String },
    /// Run one schedule now.
    Run { schedule_id: String },
    /// Run every schedule that is due, once each.
    RunDue,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EveryArg {
    Day,
    Weekday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Debug, Args)]
struct AddArgs {
    /// What to call it.
    #[arg(long)]
    title: String,
    /// What each run is asked to do.
    #[arg(long)]
    goal: String,
    /// When: every day, every weekday, or one day a week.
    #[arg(long, value_enum)]
    every: EveryArg,
    /// The local time, as HH:MM on a 24-hour clock.
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// The project to work in. Omit for a chat.
    #[arg(long, value_name = "PROJECT_ID")]
    project_id: Option<String>,
    /// Where its models may run. Defaults to local only.
    #[arg(long, value_enum)]
    mode: Option<ModeArg>,
}

fn parse_time(value: &str) -> Option<(u8, u8)> {
    let (hour, minute) = value.split_once(':')?;
    Some((hour.parse().ok()?, minute.parse().ok()?))
}

fn cadence(every: EveryArg, hour: u8, minute: u8) -> Cadence {
    let weekly = |weekday| Cadence::Weekly {
        weekday,
        hour,
        minute,
    };
    match every {
        EveryArg::Day => Cadence::Daily { hour, minute },
        EveryArg::Weekday => Cadence::Weekdays { hour, minute },
        EveryArg::Monday => weekly(Weekday::Monday),
        EveryArg::Tuesday => weekly(Weekday::Tuesday),
        EveryArg::Wednesday => weekly(Weekday::Wednesday),
        EveryArg::Thursday => weekly(Weekday::Thursday),
        EveryArg::Friday => weekly(Weekday::Friday),
        EveryArg::Saturday => weekly(Weekday::Saturday),
        EveryArg::Sunday => weekly(Weekday::Sunday),
    }
}

pub(crate) async fn execute(args: ScheduleArgs) -> ExitCode {
    if let ScheduleCommand::Add(add) = &args.command
        && parse_time(&add.at).is_none()
    {
        eprintln!("error: --at takes a time as HH:MM, such as 08:00");
        return ExitCode::FAILURE;
    }
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let rendered = match args.command {
        ScheduleCommand::List => service.list_schedules().await.map(|all| render::json(&all)),
        ScheduleCommand::Add(add) => {
            let (hour, minute) = parse_time(&add.at).unwrap_or_default();
            service
                .create_schedule(&NewSchedule {
                    title: add.title,
                    goal: add.goal,
                    project_id: add.project_id.map(ProjectId::new),
                    cadence: cadence(add.every, hour, minute),
                    routing_mode: add.mode.map_or(RoutingMode::LocalOnly, Into::into),
                })
                .await
                .map(|created| render::json(&created))
        }
        ScheduleCommand::Enable { schedule_id } => service
            .set_schedule_enabled(&schedule_id, true)
            .await
            .map(|schedule| render::json(&schedule)),
        ScheduleCommand::Disable { schedule_id } => service
            .set_schedule_enabled(&schedule_id, false)
            .await
            .map(|schedule| render::json(&schedule)),
        ScheduleCommand::Remove { schedule_id } => service
            .delete_schedule(&schedule_id)
            .await
            .map(|()| ExitCode::SUCCESS),
        ScheduleCommand::Run { schedule_id } => service
            .run_schedule_now(&schedule_id)
            .await
            .map(|task| render::json(&task)),
        ScheduleCommand::RunDue => service
            .run_due_schedules()
            .await
            .map(|started| render::json(&started)),
    };
    rendered.unwrap_or_else(|error| render::application_error(&error))
}
