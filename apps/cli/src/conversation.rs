//! Durable conversation commands.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use tramontana_core::contracts::{ConversationId, ProjectId};

use crate::{render, wiring};

/// Arguments for `tramontana conversation`.
#[derive(Debug, Args)]
pub(crate) struct ConversationArgs {
    #[command(subcommand)]
    command: ConversationCommand,
}

#[derive(Debug, Subcommand)]
enum ConversationCommand {
    /// Create a durable conversation, optionally inside a project.
    Create(CreateConversationArgs),
    /// List project conversations, or projectless conversations when omitted.
    List(ListConversationArgs),
    /// Inspect one durable conversation.
    Inspect(InspectConversationArgs),
    /// Give a conversation a new title.
    Rename(RenameConversationArgs),
    /// Delete a conversation and every task, event and checkpoint recorded
    /// under it. This cannot be undone.
    Delete(DeleteConversationArgs),
    /// Search conversation titles, what was asked in them, and project names
    /// and folders.
    Search(SearchArgs),
}

#[derive(Debug, Args)]
struct RenameConversationArgs {
    #[arg(value_name = "CONVERSATION_ID")]
    conversation_id: String,

    /// The new title.
    title: String,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct DeleteConversationArgs {
    #[arg(value_name = "CONVERSATION_ID")]
    conversation_id: String,

    /// Confirm the deletion. Without it, nothing is deleted.
    #[arg(long)]
    yes: bool,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SearchArgs {
    /// What to look for, ignoring case.
    query: String,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CreateConversationArgs {
    /// Conversation title.
    #[arg(default_value = "New conversation")]
    title: String,

    /// Project to attach this conversation to.
    #[arg(long, value_name = "PROJECT_ID")]
    project_id: Option<String>,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ListConversationArgs {
    /// Project whose conversations should be listed. Omit for projectless ones.
    #[arg(long, value_name = "PROJECT_ID")]
    project_id: Option<String>,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct InspectConversationArgs {
    #[arg(value_name = "CONVERSATION_ID")]
    conversation_id: String,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

pub(crate) async fn execute(args: ConversationArgs) -> ExitCode {
    match args.command {
        ConversationCommand::Create(args) => create(args).await,
        ConversationCommand::List(args) => list(args).await,
        ConversationCommand::Inspect(args) => inspect(args).await,
        ConversationCommand::Rename(args) => rename(args).await,
        ConversationCommand::Delete(args) => delete(args).await,
        ConversationCommand::Search(args) => search(args).await,
    }
}

async fn rename(args: RenameConversationArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    match service
        .rename_conversation(&ConversationId::new(args.conversation_id), &args.title)
        .await
    {
        Ok(conversation) => render::json(&conversation),
        Err(error) => render::application_error(&error),
    }
}

async fn delete(args: DeleteConversationArgs) -> ExitCode {
    if !args.yes {
        eprintln!(
            "error: deleting a conversation removes every task, event and checkpoint recorded under it, and cannot be undone; pass --yes to confirm"
        );
        return ExitCode::FAILURE;
    }
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    match service
        .delete_conversation(&ConversationId::new(args.conversation_id))
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => render::application_error(&error),
    }
}

async fn search(args: SearchArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    match service.search(&args.query).await {
        Ok(results) => render::json(&results),
        Err(error) => render::application_error(&error),
    }
}

async fn create(args: CreateConversationArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let project_id = args.project_id.map(ProjectId::new);
    match service.create_conversation(project_id, args.title).await {
        Ok(conversation) => render::json(&conversation),
        Err(error) => render::application_error(&error),
    }
}

async fn list(args: ListConversationArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let project_id = args.project_id.map(ProjectId::new);
    match service.list_conversations(project_id.as_ref()).await {
        Ok(conversations) => render::json(&conversations),
        Err(error) => render::application_error(&error),
    }
}

async fn inspect(args: InspectConversationArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    match service
        .get_conversation(&ConversationId::new(args.conversation_id))
        .await
    {
        Ok(conversation) => render::json(&conversation),
        Err(error) => render::application_error(&error),
    }
}
