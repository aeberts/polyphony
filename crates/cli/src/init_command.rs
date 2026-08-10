use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use serde::Serialize;

use crate::{bootstrap_support::workflow_root_dir, errors::format_fatal_error, prelude::*, *};

const MULTI_AGENT_TEMPLATE: &str =
    include_str!("../../../templates/examples/WORKFLOW.multi-agent.md");
const CODEX_SHORTHAND_TEMPLATE: &str =
    include_str!("../../../templates/examples/WORKFLOW.codex-shorthand.md");
const PIPELINE_STATIC_TEMPLATE: &str =
    include_str!("../../../templates/examples/WORKFLOW.pipeline-static.md");
const PIPELINE_PLANNER_TEMPLATE: &str =
    include_str!("../../../templates/examples/WORKFLOW.pipeline-planner.md");
const CLOSED_LOOP_DELIVERY_TEMPLATE: &str =
    include_str!("../../../templates/examples/WORKFLOW.closed-loop-delivery.md");
const AUTOMATION_FEEDBACK_TEMPLATE: &str =
    include_str!("../../../templates/examples/WORKFLOW.automation-feedback.md");
const ALL_INIT_TEMPLATES: [InitTemplate; 7] = [
    InitTemplate::Default,
    InitTemplate::Codex,
    InitTemplate::MultiAgent,
    InitTemplate::PipelineStatic,
    InitTemplate::PipelinePlanner,
    InitTemplate::ClosedLoopDelivery,
    InitTemplate::AutomationFeedback,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub(crate) enum InitTemplate {
    #[default]
    Default,
    Codex,
    MultiAgent,
    PipelineStatic,
    PipelinePlanner,
    ClosedLoopDelivery,
    AutomationFeedback,
}

impl InitTemplate {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Codex => "codex",
            Self::MultiAgent => "multi-agent",
            Self::PipelineStatic => "pipeline-static",
            Self::PipelinePlanner => "pipeline-planner",
            Self::ClosedLoopDelivery => "closed-loop-delivery",
            Self::AutomationFeedback => "automation-feedback",
        }
    }

    fn contents(self) -> &'static str {
        match self {
            Self::Default => polyphony_workflow::default_workflow_md(),
            Self::Codex => CODEX_SHORTHAND_TEMPLATE,
            Self::MultiAgent => MULTI_AGENT_TEMPLATE,
            Self::PipelineStatic => PIPELINE_STATIC_TEMPLATE,
            Self::PipelinePlanner => PIPELINE_PLANNER_TEMPLATE,
            Self::ClosedLoopDelivery => CLOSED_LOOP_DELIVERY_TEMPLATE,
            Self::AutomationFeedback => AUTOMATION_FEEDBACK_TEMPLATE,
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Default => "Annotated baseline workflow with the full reference surface.",
            Self::Codex => {
                "Single-agent Codex app-server workflow with the legacy `codex:` shorthand."
            },
            Self::MultiAgent => {
                "Routing-oriented workflow with multiple providers and fallback chains."
            },
            Self::PipelineStatic => "Fixed research -> coding -> review pipeline for every issue.",
            Self::PipelinePlanner => {
                "Planner-driven pipeline that decomposes work before execution."
            },
            Self::ClosedLoopDelivery => {
                "Bounded implementation -> independent QA -> repair delivery loop."
            },
            Self::AutomationFeedback => {
                "Automation-first workflow with feedback channels and PR handoff."
            },
        }
    }

    fn when_to_use(self) -> &'static str {
        match self {
            Self::Default => {
                "Use when you want the full reference file and plan to shape it yourself."
            },
            Self::Codex => {
                "Use when Codex is your primary engine and you want the simplest viable start."
            },
            Self::MultiAgent => {
                "Use when different issue states or workloads should route to different agents."
            },
            Self::PipelineStatic => "Use when every issue should follow the same handoff sequence.",
            Self::PipelinePlanner => {
                "Use when issues vary a lot and you want a planner to decide the task breakdown."
            },
            Self::ClosedLoopDelivery => {
                "Use for explicitly approved issues that need independent QA and at most two repairs."
            },
            Self::AutomationFeedback => {
                "Use when Polyphony should open PRs, notify humans, and hand off automatically."
            },
        }
    }

    fn validation_note(self) -> Option<&'static str> {
        match self {
            Self::PipelineStatic | Self::PipelinePlanner | Self::AutomationFeedback => Some(
                "This starter enables automation. You will need to wire a real tracker before validation fully passes.",
            ),
            Self::ClosedLoopDelivery => Some(
                "This starter requires a real tracker because implementation, QA, and repair evidence are tracker comments.",
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum InitTracker {
    Auto,
    None,
    Github,
    Gitlab,
    Linear,
    Beads,
}

impl InitTracker {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Linear => "linear",
            Self::Beads => "beads",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitOptions {
    pub(crate) pack: InitTemplate,
    pub(crate) force: bool,
    pub(crate) tracker: InitTracker,
    pub(crate) repository: Option<String>,
    pub(crate) project_slug: Option<String>,
    pub(crate) default_branch: Option<String>,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            pack: InitTemplate::Default,
            force: false,
            tracker: InitTracker::Auto,
            repository: None,
            project_slug: None,
            default_branch: None,
        }
    }
}

impl InitOptions {
    fn requires_repo_seed(&self) -> bool {
        self.tracker != InitTracker::Auto
            || self.repository.is_some()
            || self.project_slug.is_some()
            || self.default_branch.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkflowWriteAction {
    Created,
    Overwritten,
    SkippedExisting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InitReport {
    pub(crate) pack: String,
    pub(crate) workflow_path: PathBuf,
    pub(crate) workflow_action: WorkflowWriteAction,
    pub(crate) user_config_path: PathBuf,
    pub(crate) user_config_created: bool,
    pub(crate) repo_config_path: Option<PathBuf>,
    pub(crate) repo_config_created: bool,
    pub(crate) repo_config_auto_detected_kind: Option<String>,
    pub(crate) tracker_needs_manual_setup: bool,
    pub(crate) created_agent_prompt_files: Vec<PathBuf>,
    pub(crate) detected_agents: Vec<String>,
    pub(crate) validation_error: Option<String>,
    pub(crate) setup_hints: Vec<String>,
}

pub(crate) fn run_init_command(
    workflow_path: &Path,
    options: &InitOptions,
) -> Result<InitReport, Error> {
    let user_config_path = user_config_path()?;
    run_init_command_with_user_config_path(workflow_path, &user_config_path, options)
}

pub(crate) fn run_init_command_with_user_config_path(
    workflow_path: &Path,
    user_config_path: &Path,
    options: &InitOptions,
) -> Result<InitReport, Error> {
    let detected_agents = polyphony_workflow::detect_agents();
    let agents_for_user_config = if options.pack == InitTemplate::Codex {
        &[][..]
    } else {
        detected_agents.as_slice()
    };
    let user_config_created = polyphony_workflow::ensure_user_config_file_with_agents(
        user_config_path,
        agents_for_user_config,
    )?;

    let workflow_action = write_workflow_template(workflow_path, options.pack, options.force)?;
    let created_agent_prompt_files = ensure_repo_agent_prompt_files(workflow_path)?;
    let default_agent = detected_agents
        .first()
        .map(|agent| agent.profile_name.as_str());
    let repo_seed = seed_repo_config_for_init(workflow_path, options.pack, default_agent, options)?;

    let validation_error = load_workflow_with_user_config(workflow_path, Some(user_config_path))
        .err()
        .map(|error| format_fatal_error(&Error::Workflow(error)));
    let setup_hints = collect_setup_hints(workflow_path, options.pack, &repo_seed)?;

    Ok(InitReport {
        pack: options.pack.label().to_string(),
        workflow_path: workflow_path.to_path_buf(),
        workflow_action,
        user_config_path: user_config_path.to_path_buf(),
        user_config_created,
        repo_config_path: repo_seed.repo_config_path,
        repo_config_created: repo_seed.repo_config_created,
        repo_config_auto_detected_kind: repo_seed.repo_config_auto_detected_kind,
        tracker_needs_manual_setup: repo_seed.tracker_needs_manual_setup,
        created_agent_prompt_files,
        detected_agents: detected_agents
            .into_iter()
            .map(|agent| agent.profile_name)
            .collect(),
        validation_error,
        setup_hints,
    })
}

pub(crate) fn print_init_report(report: &InitReport) {
    match report.workflow_action {
        WorkflowWriteAction::Created => {
            println!(
                "Created {} from the `{}` pack.",
                report.workflow_path.display(),
                report.pack
            );
        },
        WorkflowWriteAction::Overwritten => {
            println!(
                "Overwrote {} with the `{}` pack.",
                report.workflow_path.display(),
                report.pack
            );
        },
        WorkflowWriteAction::SkippedExisting => {
            println!("Kept existing {}.", report.workflow_path.display());
        },
    }

    if let Some(selected) = ALL_INIT_TEMPLATES
        .into_iter()
        .find(|template| template.label() == report.pack)
    {
        println!("Pack: {}.", selected.summary());
        println!("Use it when: {}.", selected.when_to_use());
    }

    if report.user_config_created {
        println!(
            "Created user config at {}.",
            report.user_config_path.display()
        );
    }

    if !report.detected_agents.is_empty() {
        println!("Detected agents: {}.", report.detected_agents.join(", "));
    } else {
        println!("No supported local agents detected yet.");
    }

    if !report.created_agent_prompt_files.is_empty() {
        println!(
            "Seeded {} repo agent prompt file(s) under {}.",
            report.created_agent_prompt_files.len(),
            report
                .workflow_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(".polyphony/agents")
                .display()
        );
    }

    if report.repo_config_created {
        if let Some(path) = &report.repo_config_path {
            if let Some(kind) = &report.repo_config_auto_detected_kind {
                println!("Created {} with {} tracker wiring.", path.display(), kind);
            } else {
                println!("Created {}.", path.display());
            }
        }
    } else if let Some(path) = &report.repo_config_path
        && path.exists()
    {
        println!("Using existing {}.", path.display());
    }

    if let Some(error) = &report.validation_error {
        println!();
        println!("The generated config still needs edits before runtime validation will pass:");
        for line in error.lines() {
            println!("  {line}");
        }
    }

    if !report.setup_hints.is_empty() {
        println!();
        println!("Setup hints:");
        for hint in &report.setup_hints {
            println!("  - {hint}");
        }
    }

    println!();
    println!("Next steps:");
    println!("  1. Review {}.", report.workflow_path.display());
    if report.tracker_needs_manual_setup {
        println!("  2. Edit polyphony.toml to choose a tracker for this repo.");
        println!("  3. Run `polyphony doctor`.");
        println!("  4. Run `polyphony`.");
    } else if report.validation_error.is_some() {
        println!(
            "  2. Adjust WORKFLOW.md, polyphony.toml, or your user config until validation passes."
        );
        println!("  3. Run `polyphony doctor`.");
        println!("  4. Run `polyphony`.");
    } else {
        println!("  2. Run `polyphony doctor`.");
        println!("  3. Run `polyphony`.");
    }
}

fn write_workflow_template(
    workflow_path: &Path,
    template: InitTemplate,
    force: bool,
) -> Result<WorkflowWriteAction, Error> {
    let existed_before = workflow_path.exists();
    if existed_before {
        if !workflow_path.is_file() {
            return Err(Error::Config(format!(
                "workflow path `{}` exists but is not a file",
                workflow_path.display()
            )));
        }
        if !force {
            return Ok(WorkflowWriteAction::SkippedExisting);
        }
    }

    ensure_parent_dir(workflow_path)?;
    fs::write(
        workflow_path,
        normalize_template_contents(template.contents()),
    )
    .map_err(|error| {
        Error::Config(format!(
            "writing `{}` failed: {error}",
            workflow_path.display()
        ))
    })?;
    Ok(if existed_before {
        WorkflowWriteAction::Overwritten
    } else {
        WorkflowWriteAction::Created
    })
}

fn ensure_parent_dir(path: &Path) -> Result<(), Error> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|error| {
        Error::Config(format!(
            "creating `{}` for workflow path failed: {error}",
            parent.display()
        ))
    })
}

fn normalize_template_contents(contents: &str) -> String {
    let normalized = contents
        .lines()
        .filter(|line| !line.trim_start().starts_with("# Destination:"))
        .collect::<Vec<_>>();
    let mut rendered = normalized.join("\n");
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered
}

fn detect_repo_config_kind(path: &Path) -> Result<Option<String>, Error> {
    let contents = fs::read_to_string(path)
        .map_err(|error| Error::Config(format!("reading `{}` failed: {error}", path.display())))?;
    let mut in_tracker_block = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_tracker_block = trimmed == "[tracker]";
            continue;
        }
        if !in_tracker_block || !trimmed.starts_with("kind") {
            continue;
        }
        let Some((_, value)) = trimmed.split_once('=') else {
            continue;
        };
        return Ok(Some(value.trim().trim_matches('"').to_string()));
    }
    Ok(None)
}

#[derive(Debug)]
struct RepoConfigSeedResult {
    repo_config_path: Option<PathBuf>,
    repo_config_created: bool,
    repo_config_auto_detected_kind: Option<String>,
    tracker_needs_manual_setup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoConfigTrackerSeed {
    kind: InitTracker,
    repository: Option<String>,
    project_slug: Option<String>,
    default_branch: Option<String>,
    endpoint: Option<String>,
    tracker_needs_manual_setup: bool,
}

fn seed_repo_config_for_init(
    workflow_path: &Path,
    template: InitTemplate,
    default_agent: Option<&str>,
    options: &InitOptions,
) -> Result<RepoConfigSeedResult, Error> {
    let workflow_root = workflow_root_dir(workflow_path)?;
    if !workflow_root.join(".git").exists() {
        return Ok(RepoConfigSeedResult {
            repo_config_path: None,
            repo_config_created: false,
            repo_config_auto_detected_kind: None,
            tracker_needs_manual_setup: false,
        });
    }

    let repo_config = repo_config_path(workflow_path)?;
    if repo_config.exists() {
        return Ok(RepoConfigSeedResult {
            repo_config_path: Some(repo_config.clone()),
            repo_config_created: false,
            repo_config_auto_detected_kind: detect_repo_config_kind(&repo_config)?,
            tracker_needs_manual_setup: false,
        });
    }

    if !should_seed_repo_config_for_init(workflow_path)? && !options.requires_repo_seed() {
        return Ok(RepoConfigSeedResult {
            repo_config_path: None,
            repo_config_created: false,
            repo_config_auto_detected_kind: None,
            tracker_needs_manual_setup: false,
        });
    }

    let source_repo_path = workflow_root
        .canonicalize()
        .unwrap_or_else(|_| workflow_root.clone());
    let tracker_seed = determine_tracker_seed(&workflow_root, options);
    write_seeded_repo_config(
        &repo_config,
        &source_repo_path,
        template,
        default_agent,
        &tracker_seed,
    )?;
    Ok(RepoConfigSeedResult {
        repo_config_path: Some(repo_config),
        repo_config_created: true,
        repo_config_auto_detected_kind: Some(tracker_seed.kind.label().to_string()),
        tracker_needs_manual_setup: tracker_seed.tracker_needs_manual_setup,
    })
}

fn determine_tracker_seed(workflow_root: &Path, options: &InitOptions) -> RepoConfigTrackerSeed {
    let beads_detected = workflow_root.join(".beads").is_dir();
    let github_repo = polyphony_git::detect_github_remote(workflow_root);
    let gitlab_remote = polyphony_git::detect_gitlab_remote(workflow_root);

    let inferred_kind = match options.tracker {
        InitTracker::Auto => {
            if beads_detected {
                InitTracker::Beads
            } else if github_repo.is_some() {
                InitTracker::Github
            } else if gitlab_remote.is_some() {
                InitTracker::Gitlab
            } else {
                InitTracker::None
            }
        },
        explicit => explicit,
    };

    let mut seed = RepoConfigTrackerSeed {
        kind: inferred_kind,
        repository: options.repository.clone(),
        project_slug: options.project_slug.clone(),
        default_branch: options.default_branch.clone(),
        endpoint: None,
        tracker_needs_manual_setup: false,
    };

    match inferred_kind {
        InitTracker::Github => {
            if seed.repository.is_none() {
                seed.repository = github_repo;
            }
            seed.tracker_needs_manual_setup = seed
                .repository
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty();
        },
        InitTracker::Gitlab => {
            if let Some((endpoint, repository)) = gitlab_remote {
                if seed.endpoint.is_none() {
                    seed.endpoint = Some(endpoint);
                }
                if seed.repository.is_none() {
                    seed.repository = Some(repository);
                }
            }
            if seed.project_slug.is_some() && seed.repository.is_none() {
                seed.repository = seed.project_slug.clone();
            }
            seed.tracker_needs_manual_setup = seed
                .repository
                .as_deref()
                .or(seed.project_slug.as_deref())
                .unwrap_or_default()
                .trim()
                .is_empty();
        },
        InitTracker::Linear => {
            seed.tracker_needs_manual_setup = seed
                .project_slug
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty();
        },
        InitTracker::Beads | InitTracker::None | InitTracker::Auto => {
            seed.tracker_needs_manual_setup = inferred_kind == InitTracker::None;
        },
    }

    seed
}

fn should_seed_repo_config_for_init(workflow_path: &Path) -> Result<bool, Error> {
    let raw = fs::read_to_string(workflow_path).map_err(|error| {
        Error::Config(format!(
            "reading `{}` failed: {error}",
            workflow_path.display()
        ))
    })?;
    let tracker_kind = front_matter_value(&raw, "tracker", "kind");
    let checkout_kind = front_matter_value(&raw, "workspace", "checkout_kind");
    let source_repo_path = front_matter_value(&raw, "workspace", "source_repo_path");
    let clone_url = front_matter_value(&raw, "workspace", "clone_url");

    Ok(tracker_kind.as_deref() == Some("none")
        || (checkout_kind.as_deref() == Some("directory")
            && source_repo_path.is_none()
            && clone_url.is_none()))
}

fn front_matter_value(raw: &str, section: &str, key: &str) -> Option<String> {
    let (_, rest) = raw.split_once("---\n")?;
    let (front_matter, _) = rest.split_once("\n---")?;
    let mut in_section = false;

    for line in front_matter.lines() {
        let trimmed_end = line.trim_end();
        let trimmed = trimmed_end.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !line.starts_with([' ', '\t']) && trimmed.ends_with(':') {
            in_section = trimmed.trim_end_matches(':') == section;
            continue;
        }
        if !in_section || !line.starts_with([' ', '\t']) {
            continue;
        }

        let Some((candidate_key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if candidate_key.trim() != key {
            continue;
        }
        return Some(value.trim().trim_matches('"').to_string());
    }

    None
}

pub(crate) fn render_template_catalog() -> String {
    let mut lines = Vec::new();
    lines.push("Available `polyphony init` packs:".to_string());
    for template in ALL_INIT_TEMPLATES {
        lines.push(format!("  {}: {}", template.label(), template.summary()));
        lines.push(format!("    Use it when: {}", template.when_to_use()));
        if let Some(note) = template.validation_note() {
            lines.push(format!("    Note: {note}"));
        }
    }
    lines.push(String::new());
    lines.push("Optional setup flags:".to_string());
    lines.push("  --tracker <auto|none|github|gitlab|linear|beads>".to_string());
    lines.push("  --repository <owner/repo>".to_string());
    lines.push("  --project-slug <ENG>".to_string());
    lines.push("  --default-branch <main>".to_string());
    lines.join("\n")
}

pub(crate) fn render_pack_catalog() -> String {
    render_template_catalog()
}

pub(crate) fn print_pack_catalog() {
    println!("{}", render_pack_catalog());
}

fn collect_setup_hints(
    workflow_path: &Path,
    template: InitTemplate,
    repo_seed: &RepoConfigSeedResult,
) -> Result<Vec<String>, Error> {
    let workflow_raw = fs::read_to_string(workflow_path).map_err(|error| {
        Error::Config(format!(
            "reading `{}` failed: {error}",
            workflow_path.display()
        ))
    })?;
    let repo_config_raw = repo_seed
        .repo_config_path
        .as_ref()
        .filter(|path| path.exists())
        .map(|path| {
            fs::read_to_string(path).map_err(|error| {
                Error::Config(format!("reading `{}` failed: {error}", path.display()))
            })
        })
        .transpose()?;

    let tracker_kind = repo_config_raw
        .as_deref()
        .and_then(|raw| toml_section_value(raw, "tracker", "kind"))
        .or_else(|| front_matter_value(&workflow_raw, "tracker", "kind"))
        .unwrap_or_else(|| "none".into());
    let tracker_repository = repo_config_raw
        .as_deref()
        .and_then(|raw| toml_section_value(raw, "tracker", "repository"))
        .or_else(|| front_matter_value(&workflow_raw, "tracker", "repository"));
    let tracker_project_slug = repo_config_raw
        .as_deref()
        .and_then(|raw| toml_section_value(raw, "tracker", "project_slug"))
        .or_else(|| front_matter_value(&workflow_raw, "tracker", "project_slug"));
    let tracker_api_key = repo_config_raw
        .as_deref()
        .and_then(|raw| toml_section_value(raw, "tracker", "api_key"))
        .or_else(|| front_matter_value(&workflow_raw, "tracker", "api_key"));
    let automation_enabled =
        front_matter_bool(&workflow_raw, "automation", "enabled").unwrap_or(matches!(
            template,
            InitTemplate::PipelineStatic
                | InitTemplate::PipelinePlanner
                | InitTemplate::AutomationFeedback
        ));
    let feedback_offered = front_matter_list_values(&workflow_raw, "feedback", "offered");
    let mut hints = Vec::new();

    if repo_seed.tracker_needs_manual_setup {
        hints.push(
            "Edit `polyphony.toml` and replace the seeded tracker placeholder with the tracker this repo actually uses, or rerun `polyphony init --pack ... --tracker ... --force`.".into(),
        );
    }

    match tracker_kind.as_str() {
        "none" if automation_enabled => hints.push(
            "This starter enables PR automation. Set `[tracker] kind = \"github\"` and `repository = \"owner/repo\"`, or disable `[automation] enabled`.".into(),
        ),
        "github" => {
            if tracker_repository.as_deref().unwrap_or_default().trim().is_empty() {
                hints.push(
                    "Add `[tracker] repository = \"owner/repo\"` so GitHub issue and PR integration knows which repository to target.".into(),
                );
            }
            if automation_enabled
                && !tracker_api_key_configured(&tracker_api_key)
                && env::var("GITHUB_TOKEN").ok().filter(|value| !value.is_empty()).is_none()
                && env::var("GH_TOKEN").ok().filter(|value| !value.is_empty()).is_none()
            {
                hints.push(
                    "GitHub automation needs a token. Export `GITHUB_TOKEN` or `GH_TOKEN`, or set `[tracker] api_key = \"$GITHUB_TOKEN\"`.".into(),
                );
            }
        },
        "gitlab" => {
            if tracker_repository
                .as_deref()
                .or(tracker_project_slug.as_deref())
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                hints.push(
                    "Add `[tracker] repository = \"group/project\"` or `project_slug = \"group/project\"` for GitLab issue lookup.".into(),
                );
            }
            if !tracker_api_key_configured(&tracker_api_key)
                && env::var("GITLAB_TOKEN")
                    .ok()
                    .filter(|value| !value.is_empty())
                    .is_none()
            {
                hints.push(
                    "GitLab API access needs a token. Export `GITLAB_TOKEN` or set `[tracker] api_key = \"$GITLAB_TOKEN\"`.".into(),
                );
            }
            if automation_enabled {
                hints.push(
                    "This starter currently requires GitHub for PR automation. For GitLab repos, disable `[automation] enabled` or switch to a non-automation starter.".into(),
                );
            }
        },
        "linear" => {
            if tracker_project_slug.as_deref().unwrap_or_default().trim().is_empty() {
                hints.push(
                    "Add `[tracker] project_slug = \"ENG\"` so Linear polling knows which project to watch.".into(),
                );
            }
            if !tracker_api_key_configured(&tracker_api_key)
                && env::var("LINEAR_API_KEY")
                    .ok()
                    .filter(|value| !value.is_empty())
                    .is_none()
            {
                hints.push(
                    "Linear needs an API key. Export `LINEAR_API_KEY` or set `[tracker] api_key = \"$LINEAR_API_KEY\"`.".into(),
                );
            }
        },
        _ => {},
    }

    if feedback_offered.iter().any(|item| item == "telegram")
        && env::var("TELEGRAM_BOT_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .is_none()
    {
        hints.push(
            "Telegram feedback is configured with `$TELEGRAM_BOT_TOKEN`. Export that env var or remove the `feedback.telegram` block.".into(),
        );
    }

    if feedback_offered.iter().any(|item| item == "webhook")
        && workflow_raw.contains("$HANDOFF_WEBHOOK_TOKEN")
        && env::var("HANDOFF_WEBHOOK_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .is_none()
    {
        hints.push(
            "Webhook feedback references `$HANDOFF_WEBHOOK_TOKEN`. Export it before sending signed handoff callbacks, or remove the bearer token field.".into(),
        );
    }

    Ok(hints)
}

fn tracker_api_key_configured(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
}

fn front_matter_bool(raw: &str, section: &str, key: &str) -> Option<bool> {
    match front_matter_value(raw, section, key)?.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn front_matter_list_values(raw: &str, section: &str, key: &str) -> Vec<String> {
    let Some((_, rest)) = raw.split_once("---\n") else {
        return Vec::new();
    };
    let Some((front_matter, _)) = rest.split_once("\n---") else {
        return Vec::new();
    };
    let mut in_section = false;
    let mut in_list = false;
    let mut values = Vec::new();

    for line in front_matter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t']) && trimmed.ends_with(':') {
            in_section = trimmed.trim_end_matches(':') == section;
            in_list = false;
            continue;
        }
        if !in_section {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            in_list = false;
            continue;
        }
        if trimmed == format!("{key}:") {
            in_list = true;
            continue;
        }
        if in_list {
            if let Some(value) = trimmed.strip_prefix("- ") {
                values.push(value.trim().trim_matches('"').to_string());
                continue;
            }
            if !trimmed.starts_with('-') {
                break;
            }
        }
    }

    values
}

fn toml_section_value(raw: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            in_section = trimmed == format!("[{section}]");
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((candidate_key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if candidate_key.trim() != key {
            continue;
        }
        return Some(value.trim().trim_matches('"').to_string());
    }
    None
}

fn write_seeded_repo_config(
    path: &Path,
    source_repo_path: &Path,
    template: InitTemplate,
    default_agent: Option<&str>,
    tracker_seed: &RepoConfigTrackerSeed,
) -> Result<(), Error> {
    let mut content = if template == InitTemplate::Codex {
        codex_repo_config_toml(source_repo_path)
    } else {
        polyphony_workflow::default_repo_config_toml_with_default_agent(
            source_repo_path,
            default_agent,
        )
    };
    content = content.replace(
        "kind = \"none\"",
        &render_tracker_seed_replacement(tracker_seed),
    );
    if let Some(default_branch) = &tracker_seed.default_branch {
        content = content.replace(
            "# default_branch = \"main\"",
            &format!("default_branch = \"{default_branch}\""),
        );
    }

    fs::write(path, content)
        .map_err(|error| Error::Config(format!("writing `{}` failed: {error}", path.display())))
}

fn render_tracker_seed_replacement(tracker_seed: &RepoConfigTrackerSeed) -> String {
    match tracker_seed.kind {
        InitTracker::Auto | InitTracker::None => "kind = \"none\"".into(),
        InitTracker::Beads => {
            "kind = \"beads\"\nactive_states = [\"Open\", \"In Progress\", \"Blocked\"]\nterminal_states = [\"Closed\", \"Deferred\"]".into()
        },
        InitTracker::Github => {
            let mut lines = vec!["kind = \"github\"".to_string()];
            if let Some(repository) = &tracker_seed.repository {
                lines.push(format!("repository = \"{repository}\""));
            }
            lines.push("api_key = \"$GITHUB_TOKEN\"".into());
            lines.join("\n")
        },
        InitTracker::Gitlab => {
            let mut lines = vec!["kind = \"gitlab\"".to_string()];
            if let Some(endpoint) = &tracker_seed.endpoint {
                lines.push(format!("endpoint = \"{endpoint}\""));
            }
            if let Some(repository) = &tracker_seed.repository {
                lines.push(format!("repository = \"{repository}\""));
            } else if let Some(project_slug) = &tracker_seed.project_slug {
                lines.push(format!("project_slug = \"{project_slug}\""));
            }
            lines.push("api_key = \"$GITLAB_TOKEN\"".into());
            lines.join("\n")
        },
        InitTracker::Linear => {
            let mut lines = vec!["kind = \"linear\"".to_string()];
            if let Some(project_slug) = &tracker_seed.project_slug {
                lines.push(format!("project_slug = \"{project_slug}\""));
            }
            lines.push("api_key = \"$LINEAR_API_KEY\"".into());
            lines.join("\n")
        },
    }
}

fn codex_repo_config_toml(source_repo_path: &Path) -> String {
    let base = polyphony_workflow::default_repo_config_toml(source_repo_path);
    let stripped = base
        .split("\n[orchestration]\n")
        .next()
        .unwrap_or(base.as_str())
        .trim_end();
    format!("{stripped}\n")
}
