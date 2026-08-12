use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use unicode_general_category::{GeneralCategory, get_general_category};

use crate::{prelude::*, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualityBarDecision {
    Remediate,
    Defer,
    NeedsHumanDecision,
}

impl QualityBarDecision {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "remediate" => Ok(Self::Remediate),
            "defer" => Ok(Self::Defer),
            "needs human decision" => Ok(Self::NeedsHumanDecision),
            _ => Err(
                "quality-bar recommendation must be `remediate`, `defer`, or `needs human decision`"
                    .into(),
            ),
        }
    }
}

impl std::fmt::Display for QualityBarDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Remediate => "remediate",
            Self::Defer => "defer",
            Self::NeedsHumanDecision => "needs human decision",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualityBarRisk {
    FalsePass,
    LostEvidence,
    DuplicateWork,
    HumanControlBypass,
}

impl QualityBarRisk {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "false pass" => Ok(Self::FalsePass),
            "lost evidence" => Ok(Self::LostEvidence),
            "duplicate work" => Ok(Self::DuplicateWork),
            "human-control bypass" => Ok(Self::HumanControlBypass),
            _ => Err(
                "quality-bar `risks:` may contain only `false pass`, `lost evidence`, `duplicate work`, or `human-control bypass`"
                    .into(),
            ),
        }
    }
}

impl std::fmt::Display for QualityBarRisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FalsePass => "false pass",
            Self::LostEvidence => "lost evidence",
            Self::DuplicateWork => "duplicate work",
            Self::HumanControlBypass => "human-control bypass",
        })
    }
}

#[derive(Debug, Clone)]
struct QualityBarAssessment {
    realistic: bool,
    material: bool,
    risks: Vec<QualityBarRisk>,
    small_fix: bool,
    recommendation: QualityBarDecision,
    follow_up: Option<String>,
    human_override: Option<QualityBarDecision>,
}

impl QualityBarAssessment {
    fn decision(&self) -> QualityBarDecision {
        self.human_override.unwrap_or(self.recommendation)
    }

    fn durable_record(&self) -> String {
        let risks = if self.risks.is_empty() {
            "none".to_string()
        } else {
            self.risks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "quality-bar assessment: realistic={}; material={}; risks={risks}; small_fix={}; recommendation={}; follow_up={}; human_override={}; decision={}",
            self.realistic,
            self.material,
            self.small_fix,
            self.recommendation,
            self.follow_up.as_deref().unwrap_or("none"),
            self.human_override
                .map(|value| value.to_string())
                .as_deref()
                .unwrap_or("none"),
            self.decision(),
        )
    }
}

impl RuntimeService {
    /// The evidence protocol is intentionally a small fixed checklist rather
    /// than free-form agent prose.  The tracker comment is the durable human
    /// record; the matching task log survives a restart and prevents a later
    /// stage from treating an unrecorded worker success as delivery evidence.
    const EVIDENCE_FIELDS: [&str; 5] = [
        "what changed",
        "what fixed",
        "commit",
        "tests run",
        "recheck",
    ];
    /// Checklist references are deliberately small, human-authored positive
    /// integers.  The upper bound makes the grammar finite and prevents an
    /// arbitrary-length digit string from becoming an accidental identifier.
    /// It is far above the number of criteria an issue can reasonably carry,
    /// while still keeping the protocol's canonical representation explicit.
    const MAX_ACCEPTANCE_CHECK_ID: u16 = 999;
    /// The quality-bar protocol is deliberately a short, fixed checklist.
    /// It records a QA finding's practical lifecycle impact; it is not a
    /// general-purpose risk score or a substitute for a human decision.
    const QUALITY_BAR_FIELDS: [&str; 6] = [
        "realistic",
        "material",
        "risks",
        "small fix",
        "recommendation",
        "follow-up",
    ];

    /// A complete Unicode-category check prevents newly discovered or
    /// previously omitted format controls from becoming apparently nonempty
    /// checklist values.  Keep the policy narrow: ordinary visible Unicode
    /// prose remains valid; only whitespace, controls, format characters, and
    /// standalone combining marks are disregarded when deciding whether a
    /// value is meaningful.
    fn is_nonvisible(character: char) -> bool {
        character.is_whitespace()
            || matches!(
                get_general_category(character),
                GeneralCategory::Control
                    | GeneralCategory::Format
                    | GeneralCategory::NonspacingMark
                    | GeneralCategory::EnclosingMark
                    | GeneralCategory::SpacingMark
            )
    }

    /// A checklist value may use Unicode prose, but must contain at least one
    /// visible, non-control character after Unicode-aware normalization.
    fn has_meaningful_evidence_value(value: &str) -> bool {
        value
            .chars()
            .any(|character| !Self::is_nonvisible(character))
    }

    fn canonical_acceptance_check_id(value: &str) -> Result<String, String> {
        if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
            return Err("acceptance check identifiers must be canonical positive integers".into());
        }
        let number = value.parse::<u16>().map_err(|_| {
            format!(
                "acceptance check `{value}` is outside the supported 1-{} range",
                Self::MAX_ACCEPTANCE_CHECK_ID
            )
        })?;
        if number == 0 || number > Self::MAX_ACCEPTANCE_CHECK_ID || number.to_string() != value {
            return Err(format!(
                "acceptance check `{value}` must be a canonical integer in the 1-{} range",
                Self::MAX_ACCEPTANCE_CHECK_ID
            ));
        }
        Ok(number.to_string())
    }

    /// Evidence keys are deliberately a narrow, ASCII-only line grammar:
    /// `key: value`, at column zero, using one of the canonical lower-case
    /// keys.  A case or whitespace variant is rejected rather than silently
    /// being treated as prose beside a canonical (possibly contradictory) key.
    fn evidence_fields(report: &str) -> Result<BTreeMap<&str, &str>, String> {
        let mut values = BTreeMap::new();
        for line in report.lines() {
            let Some((label, candidate)) = line.split_once(':') else {
                continue;
            };
            let normalized = label
                .chars()
                .filter(|character| !character.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect::<String>();
            let canonical = Self::EVIDENCE_FIELDS
                .iter()
                .copied()
                .chain(std::iter::once("checks"))
                .find(|field| {
                    field
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .eq(normalized.chars())
                });
            let Some(field) = canonical else {
                continue;
            };
            if label != field {
                return Err(format!(
                    "evidence field `{field}` must use canonical ASCII grammar `{field}: <value>`"
                ));
            }
            let candidate = candidate.trim_matches([' ', '\t']);
            if !Self::has_meaningful_evidence_value(candidate) {
                return Err(format!(
                    "evidence field `{field}` must have a visible nonempty value"
                ));
            }
            if values.insert(field, candidate).is_some() {
                return Err(format!("evidence field `{field}` must appear exactly once"));
            }
        }
        Ok(values)
    }

    fn quality_bar_fields(report: &str) -> Result<BTreeMap<&str, &str>, String> {
        let mut values = BTreeMap::new();
        for line in report.lines() {
            let Some((label, candidate)) = line.split_once(':') else {
                continue;
            };
            let normalized = label
                .chars()
                .filter(|character| !character.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect::<String>();
            let canonical = Self::QUALITY_BAR_FIELDS.iter().copied().find(|field| {
                field
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .eq(normalized.chars())
            });
            let Some(field) = canonical else {
                continue;
            };
            if label != field {
                return Err(format!(
                    "quality-bar field `{field}` must use canonical ASCII grammar `{field}: <value>`"
                ));
            }
            let candidate = candidate.trim_matches([' ', '\t']);
            if !Self::has_meaningful_evidence_value(candidate) {
                return Err(format!(
                    "quality-bar field `{field}` must have a visible nonempty value"
                ));
            }
            if values.insert(field, candidate).is_some() {
                return Err(format!(
                    "quality-bar field `{field}` must appear exactly once"
                ));
            }
        }
        Ok(values)
    }

    fn qa_quality_bar_assessment(report: &str) -> Result<QualityBarAssessment, String> {
        let fields = Self::quality_bar_fields(report)?;
        for required in [
            "realistic",
            "material",
            "risks",
            "small fix",
            "recommendation",
        ] {
            if !fields.contains_key(required) {
                return Err(format!(
                    "QA FAIL quality-bar assessment is missing required `{required}:` field"
                ));
            }
        }
        let parse_yes_no = |field: &str| match fields[field] {
            "yes" => Ok(true),
            "no" => Ok(false),
            _ => Err(format!("quality-bar `{field}:` must be `yes` or `no`")),
        };
        let realistic = parse_yes_no("realistic")?;
        let material = parse_yes_no("material")?;
        let small_fix = parse_yes_no("small fix")?;
        let risks = Self::quality_bar_risks(fields["risks"])?;
        let recommendation = QualityBarDecision::parse(fields["recommendation"])?;
        // Automatic repair is limited to material, realistic findings with a
        // small bounded fix. Listed safety risks change whether a finding may
        // defer, but never relax the material requirement for repair.
        let expected = if material && realistic && small_fix {
            QualityBarDecision::Remediate
        } else if risks.is_empty() && !material && !realistic {
            QualityBarDecision::Defer
        } else {
            QualityBarDecision::NeedsHumanDecision
        };
        if recommendation != expected {
            return Err(format!(
                "quality-bar recommendation `{recommendation}` contradicts its lifecycle assessment; expected `{expected}`"
            ));
        }
        let follow_up = fields.get("follow-up").map(|value| (*value).to_string());
        if recommendation == QualityBarDecision::Defer && follow_up.is_none() {
            return Err("deferred hardening must include a nonempty `follow-up:` reference".into());
        }
        if recommendation != QualityBarDecision::Defer && follow_up.is_some() {
            return Err("only deferred hardening may include a `follow-up:` reference".into());
        }
        Ok(QualityBarAssessment {
            realistic,
            material,
            risks,
            small_fix,
            recommendation,
            follow_up,
            human_override: None,
        })
    }

    fn quality_bar_risks(value: &str) -> Result<Vec<QualityBarRisk>, String> {
        if value == "none" {
            return Ok(Vec::new());
        }
        let mut risks = Vec::new();
        for token in value.split(',') {
            let risk = QualityBarRisk::parse(token.trim_matches([' ', '\t']))?;
            if risks.contains(&risk) {
                return Err(format!("quality-bar `risks:` repeats `{risk}`"));
            }
            risks.push(risk);
        }
        if risks.is_empty() {
            return Err(
                "quality-bar `risks:` must be `none` or a comma-separated risk list".into(),
            );
        }
        Ok(risks)
    }

    fn quality_bar_override(issue: &Issue) -> Result<Option<QualityBarDecision>, String> {
        let mut override_decision = None;
        for comment in &issue.comments {
            for line in comment.body.lines() {
                let Some(value) = line.strip_prefix("QUALITY BAR OVERRIDE:") else {
                    continue;
                };
                let authorized_human = comment.author.as_ref().is_some_and(|author| {
                    matches!(
                        author.role.as_deref(),
                        Some("owner" | "member" | "collaborator")
                    )
                });
                if !authorized_human {
                    return Err(
                        "QUALITY BAR OVERRIDE must be authored by an owner, member, or collaborator"
                            .into(),
                    );
                }
                let decision = QualityBarDecision::parse(value.trim_matches([' ', '\t']))?;
                if override_decision.replace(decision).is_some() {
                    return Err("multiple QUALITY BAR OVERRIDE directives are ambiguous".into());
                }
            }
        }
        Ok(override_decision)
    }

    /// An acceptance source is an explicitly headed `Acceptance checks` or
    /// `Acceptance criteria` section (plain or ATX Markdown).  This narrow
    /// boundary keeps ordinary prose such as `2026 roadmap` outside that
    /// section from being mistaken for protocol input.  Once inside a marked
    /// section, every numeric/list/heading form that could be read as a
    /// criterion is input: it must be the canonical `N. description` grammar
    /// or the entire source fails closed.
    fn is_acceptance_heading(line: &str) -> bool {
        let trimmed = line.trim();
        let title = if let Some(markdown_title) = Self::markdown_heading_title(trimmed) {
            markdown_title
        } else {
            trimmed
        };
        let title = title.strip_suffix(':').unwrap_or(title).trim_end();
        title.eq_ignore_ascii_case("acceptance checks")
            || title.eq_ignore_ascii_case("acceptance criteria")
    }

    /// Returns a standard ATX Markdown heading title.  A malformed `##1.` is
    /// deliberately not treated as a section boundary; the criterion detector
    /// below sees it and rejects it instead of silently skipping its content.
    fn markdown_heading_title(line: &str) -> Option<&str> {
        let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
        if hashes == 0 {
            return None;
        }
        let remainder = line.get(hashes..)?;
        let first = remainder.chars().next()?;
        first.is_whitespace().then_some(remainder.trim())
    }

    fn is_number_sign(character: char) -> bool {
        matches!(
            get_general_category(character),
            GeneralCategory::MathSymbol | GeneralCategory::DashPunctuation
        )
    }

    fn is_criterion_separator(character: char) -> bool {
        matches!(character, '.' | ')' | ':' | '\u{ff0e}' | '\u{3002}')
    }

    /// This is intentionally broader than the accepted grammar.  It detects
    /// only forms with a criterion delimiter (or a signed number), so prose
    /// like `2026 roadmap` remains prose, while `01.`, `1)`, `- 1.`, `## 1.`,
    /// Unicode signs/digits, and bidi/zero-width-obscured markers fail closed.
    fn looks_like_acceptance_criterion(line: &str) -> bool {
        fn direct_candidate(value: &str) -> bool {
            // The accepted grammar intentionally does not normalize source
            // text.  Detection does, so controls cannot split `1` from `.`,
            // or hide a sign/digit marker and turn invalid criteria into
            // ignored prose.  The raw line is then rejected below.
            let value = value
                .chars()
                .filter(|character| !RuntimeService::is_nonvisible(*character))
                .collect::<String>();
            let Some(first) = value.chars().next() else {
                return false;
            };
            if RuntimeService::is_number_sign(first) {
                let remainder = &value[first.len_utf8()..];
                return remainder.chars().next().is_some_and(char::is_numeric);
            }
            if !first.is_numeric() {
                return false;
            }
            let numeric_end = value
                .char_indices()
                .find_map(|(index, character)| (!character.is_numeric()).then_some(index))
                .unwrap_or(value.len());
            let remainder = &value[numeric_end..];
            remainder
                .chars()
                .next()
                .is_some_and(RuntimeService::is_criterion_separator)
        }

        let trimmed = line.trim();
        if direct_candidate(trimmed) {
            return true;
        }
        let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if hashes > 0 && trimmed.get(hashes..).is_some_and(direct_candidate) {
            return true;
        }
        let Some(marker) = trimmed.chars().next() else {
            return false;
        };
        if !matches!(marker, '*' | '+' | '-') {
            return false;
        }
        let remainder = &trimmed[marker.len_utf8()..];
        remainder.chars().next().is_some_and(Self::is_nonvisible) && direct_candidate(remainder)
    }

    fn required_acceptance_checks(issue: &Issue) -> Result<Vec<String>, String> {
        let mut checks = Vec::new();
        let mut seen = BTreeSet::new();
        let mut previous = None;
        let mut in_acceptance_section = false;
        for line in issue.description.as_deref().unwrap_or_default().lines() {
            if !in_acceptance_section {
                in_acceptance_section = Self::is_acceptance_heading(line);
                continue;
            }
            let trimmed = line.trim();
            if Self::markdown_heading_title(trimmed).is_some()
                && !Self::looks_like_acceptance_criterion(trimmed)
            {
                break;
            }
            if !Self::looks_like_acceptance_criterion(trimmed) {
                continue;
            }
            let digits = trimmed
                .bytes()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            let Some(remainder) = trimmed.get(digits..) else {
                return Err(format!(
                    "acceptance check `{trimmed}` must use canonical `N. <nonempty description>` grammar"
                ));
            };
            let Some(description) = remainder.strip_prefix('.') else {
                return Err(format!(
                    "acceptance check `{trimmed}` must use canonical `N. <nonempty description>` grammar"
                ));
            };
            let number = Self::canonical_acceptance_check_id(&trimmed[..digits])?;
            if !description.starts_with([' ', '\t'])
                || !Self::has_meaningful_evidence_value(description)
            {
                return Err(format!(
                    "acceptance check `{trimmed}` must use canonical `N. <nonempty description>` grammar"
                ));
            }
            if !seen.insert(number.clone()) {
                return Err(format!("acceptance check `{number}` is duplicated"));
            }
            let number = number
                .parse::<u16>()
                .expect("canonical check id parsed above");
            if previous.is_some_and(|previous| number <= previous) {
                return Err(format!(
                    "acceptance check `{number}` must appear in strictly increasing order"
                ));
            }
            previous = Some(number);
            checks.push(number.to_string());
        }
        if in_acceptance_section && checks.is_empty() {
            return Err(
                "acceptance section must contain at least one canonical `N. <nonempty description>` check"
                    .into(),
            );
        }
        Ok(checks)
    }

    fn evidence_checks(fields: &BTreeMap<&str, &str>) -> Result<Vec<String>, String> {
        let Some(values) = fields.get("checks") else {
            return Ok(Vec::new());
        };
        let mut checks = Vec::new();
        let mut seen = BTreeSet::new();
        for token in values.split(',') {
            let token = token.trim_matches([' ', '\t']);
            let token = Self::canonical_acceptance_check_id(token).map_err(|_| {
                "`checks:` must be a comma-separated list of canonical positive integers in the supported range"
                    .to_string()
            })?;
            if !seen.insert(token.clone()) {
                return Err(format!(
                    "`checks:` contains duplicate acceptance check `{token}`"
                ));
            }
            checks.push(token);
        }
        Ok(checks)
    }

    fn require_evidence_fields(
        report: &str,
        fields: &[&str],
        role: polyphony_core::PipelineTaskRole,
    ) -> Result<(), String> {
        let mut missing = Vec::new();
        let parsed = Self::evidence_fields(report)?;
        for field in fields {
            if !parsed.contains_key(field) {
                missing.push(*field);
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} evidence is missing required checklist fields: {}",
                role,
                missing.join(", ")
            ))
        }
    }

    fn require_commit_evidence(
        report: &str,
        role: polyphony_core::PipelineTaskRole,
    ) -> Result<(), String> {
        let parsed = Self::evidence_fields(report)?;
        let commit = parsed
            .get("commit")
            .copied()
            .expect("commit field is required before its value is validated");
        // Six hexadecimal characters is the shortest abbreviated SHA accepted
        // by Git's revision parser; full hashes remain valid as well.
        let is_commit_hash = (6..=64).contains(&commit.len())
            && commit
                .chars()
                .all(|character| character.is_ascii_hexdigit());
        let is_explained_no_commit = commit
            .strip_prefix("none — ")
            .is_some_and(|reason| !reason.trim().is_empty());

        if is_commit_hash || is_explained_no_commit {
            Ok(())
        } else {
            Err(format!(
                "{role} evidence must use a 6-64 character hexadecimal commit hash or `commit: none — <nonempty reason>`"
            ))
        }
    }

    pub(crate) fn delivery_note(
        task: &Task,
        issue: &Issue,
        outcome: &AgentRunResult,
    ) -> Result<String, String> {
        let report = outcome.final_issue_state.as_deref().unwrap_or("").trim();
        let prefix = match task.role {
            polyphony_core::PipelineTaskRole::Implementation => "IMPLEMENTATION NOTE:",
            polyphony_core::PipelineTaskRole::Repair => "REPAIR NOTE:",
            polyphony_core::PipelineTaskRole::Qa => match Self::qa_verdict(outcome)? {
                // qa_verdict validates the PASS/FAIL prefix and non-empty evidence.
                (true, _) => "QA PASS:",
                (false, _) => "QA FAIL:",
            },
        };
        let Some(body) = report.strip_prefix(prefix).map(str::trim) else {
            return Err(format!(
                "{} completed without required {prefix} evidence",
                task.role
            ));
        };
        if body.is_empty() {
            return Err(format!(
                "{} completed with an empty {prefix} evidence note",
                task.role
            ));
        }
        match task.role {
            polyphony_core::PipelineTaskRole::Implementation => {
                // `commit: none — <reason>` is valid for a non-code outcome;
                // requiring the field makes that decision auditable without
                // assuming every delivery necessarily creates a commit.
                Self::require_evidence_fields(
                    body,
                    &["what changed", "commit", "tests run", "checks"],
                    task.role,
                )?;
                Self::require_commit_evidence(body, task.role)?;
            },
            polyphony_core::PipelineTaskRole::Repair => {
                Self::require_evidence_fields(
                    body,
                    &["what fixed", "commit", "tests run", "recheck", "checks"],
                    task.role,
                )?;
                Self::require_commit_evidence(body, task.role)?;
            },
            polyphony_core::PipelineTaskRole::Qa => {
                Self::require_evidence_fields(body, &["tests run", "checks"], task.role)?;
                if matches!(Self::qa_verdict(outcome)?, (false, _)) {
                    Self::qa_quality_bar_assessment(body)?;
                }
            },
        }
        let fields = Self::evidence_fields(body)?;
        let required = Self::required_acceptance_checks(issue)?;
        if !required.is_empty() {
            let covered = Self::evidence_checks(&fields)?;
            if covered.is_empty() {
                return Err(format!(
                    "{} evidence must link to at least one acceptance check using `checks: ...`",
                    task.role
                ));
            }
            let required_set = required.iter().collect::<BTreeSet<_>>();
            let out_of_range = covered
                .iter()
                .filter(|check| !required_set.contains(check))
                .cloned()
                .collect::<Vec<_>>();
            if !out_of_range.is_empty() {
                return Err(format!(
                    "evidence references acceptance checks outside the issue: {}",
                    out_of_range.join(", ")
                ));
            }
            if task.role == polyphony_core::PipelineTaskRole::Qa {
                let missing = required
                    .iter()
                    .filter(|check| !covered.contains(check))
                    .cloned()
                    .collect::<Vec<_>>();
                if !missing.is_empty() {
                    return Err(format!(
                        "QA evidence is incomplete; missing acceptance checks: {}",
                        missing.join(", ")
                    ));
                }
            }
        }
        Ok(report.to_string())
    }

    pub(crate) async fn publish_delivery_note(
        &mut self,
        issue: &Issue,
        run_id: &str,
        task: &Task,
        note: &str,
    ) -> Result<(), Error> {
        let body = Self::delivery_comment_body(run_id, task, note);
        let marker = Self::delivery_marker(run_id, task, note);
        // A worker can finish after the tracker accepted its comment but before
        // the run/task state is persisted.  On recovery the task is retried,
        // so the tracker-provided issue snapshot is the durable source of
        // truth for whether that publication already happened.
        let marker_matches = issue
            .comments
            .iter()
            .filter(|comment| comment.body.lines().any(|line| line == marker))
            .collect::<Vec<_>>();
        if marker_matches.len() > 1 {
            return Err(Error::Core(polyphony_core::Error::Adapter(format!(
                "delivery evidence marker for {} task {} is duplicated; refusing ambiguous reconciliation",
                task.role, task.id
            ))));
        }
        if let Some(comment) = marker_matches.first() {
            if comment.body != body {
                return Err(Error::Core(polyphony_core::Error::Adapter(format!(
                    "delivery evidence marker for {} task {} does not identify a complete matching note",
                    task.role, task.id
                ))));
            }
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.push_log(
                    polyphony_core::RunLogScope::Pipeline,
                    format!(
                        "evidence already recorded for {} task {}; not posting a duplicate",
                        task.role, task.id
                    ),
                );
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            return Ok(());
        }
        let comment = self
            .tracker_for_issue(&issue.id)
            .comment_on_issue(&polyphony_core::AddIssueCommentRequest {
                id: issue.id.clone(),
                body,
            })
            .await?;
        if let Some(run) = self.state.runs.get_mut(run_id) {
            run.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!(
                    "evidence recorded for {} task {}: {}",
                    task.role,
                    task.id,
                    comment.url.unwrap_or(comment.id)
                ),
            );
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }
        Ok(())
    }

    pub(crate) fn delivery_marker(run_id: &str, task: &Task, note: &str) -> String {
        let digest = Sha256::digest(note.as_bytes());
        format!(
            "<!-- polyphony:delivery-evidence-v2 run={run_id} task={} role={} sha256={digest:x} -->",
            task.id, task.role
        )
    }

    pub(crate) fn delivery_comment_body(run_id: &str, task: &Task, note: &str) -> String {
        let marker = Self::delivery_marker(run_id, task, note);
        format!(
            "{marker}\n## Polyphony {}\n\n{note}\n\nRole: `{}`\nTask: `{}`",
            match task.role {
                polyphony_core::PipelineTaskRole::Implementation => "implementation note",
                polyphony_core::PipelineTaskRole::Qa => "QA note",
                polyphony_core::PipelineTaskRole::Repair => "repair note",
            },
            task.role,
            task.id,
        )
    }

    /// QA is not allowed to silently succeed.  Its terminal state must carry a
    /// machine-readable verdict and non-empty evidence so restart/retry logic
    /// can make a durable, role-safe decision without trusting a prompt.
    fn qa_verdict(outcome: &AgentRunResult) -> Result<(bool, String), String> {
        let Some(report) = outcome.final_issue_state.as_deref() else {
            return Err("QA completed without a durable QA PASS or QA FAIL verdict".into());
        };
        let report = report.trim();
        for (prefix, passed) in [("QA PASS:", true), ("QA FAIL:", false)] {
            if let Some(evidence) = report.strip_prefix(prefix) {
                let evidence = evidence.trim();
                if !evidence.is_empty() {
                    return Ok((passed, evidence.to_string()));
                }
            }
        }
        Err("QA verdict must be `QA PASS: <evidence>` or `QA FAIL: <evidence>`".into())
    }

    pub(crate) async fn dispatch_pipeline(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        prefer_alternate_agent: bool,
        skip_workspace_sync: bool,
        directives: Option<&str>,
    ) -> Result<(), Error> {
        // A cancellation is terminal for a pipeline run.  In particular, do
        // not let a continuation/retry turn a persisted cancellation back
        // into Planning or Executing.
        if self
            .find_existing_run_for_issue(&issue.id)
            .and_then(|run_id| self.state.runs.get(&run_id))
            .is_some_and(|run| {
                matches!(run.status, RunStatus::Cancelled | RunStatus::Blocked)
                    || (run.status == RunStatus::Review
                        && run.activity_log.iter().any(|entry| {
                            entry.message.contains("closed-loop repair limit reached")
                        }))
            })
            || self.issue_is_in_blocked_state(&workflow, &issue.state)
        {
            self.push_event(
                EventScope::Dispatch,
                format!(
                    "{} pipeline dispatch skipped: run is terminal",
                    issue.identifier
                ),
            );
            return Ok(());
        }
        let manual_dispatch_directives = directives
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned);
        let workspace_manager = if skip_workspace_sync {
            info!(
                issue_identifier = %issue.identifier,
                "resuming orphaned workspace without sync_on_reuse"
            );
            WorkspaceManager::new(
                workflow.config.workspace.root.clone(),
                self.provisioner.clone(),
                workflow.config.workspace.checkout_kind,
                false,
                workflow.config.workspace.transient_paths.clone(),
                workflow.config.workspace.source_repo_path.clone(),
                workflow.config.workspace.clone_url.clone(),
                workflow.config.workspace.default_branch.clone(),
            )
        } else {
            self.build_workspace_manager(&workflow)
        };
        let workspace = workspace_manager
            .ensure_workspace(
                &issue.identifier,
                issue.branch_name.clone().or_else(|| {
                    Some(format!(
                        "task/{}",
                        sanitize_workspace_key(&issue.identifier)
                    ))
                }),
                &workflow.config.hooks,
            )
            .await?;
        self.state
            .worktree_keys
            .insert(workspace.workspace_key.clone());

        let tracker = self.tracker_for_issue(&issue.id);
        if !is_synthetic_issue_id(&issue.id) {
            if let Err(error) = tracker.ensure_issue_workflow_tracking(&issue).await {
                warn!(%error, issue_identifier = %issue.identifier, "issue workflow tracking setup failed");
            }
            if let Err(error) = tracker
                .update_issue_workflow_status(&issue, "In Progress")
                .await
            {
                warn!(%error, issue_identifier = %issue.identifier, "issue workflow status sync failed");
            }
        }

        let has_planner = workflow.config.router_agent_name().is_some();
        let initial_status = if has_planner {
            RunStatus::Planning
        } else {
            RunStatus::InProgress
        };
        // Reuse an existing active run for this issue if one exists,
        // otherwise create a new one.
        let (run_id, existing_stage) =
            if let Some(existing_id) = self.find_existing_run_for_issue(&issue.id) {
                let stage = self
                    .state
                    .runs
                    .get(&existing_id)
                    .and_then(|m| m.pipeline_stage);
                // Determine what status this run should have on reuse.
                let reuse_status = match stage {
                    Some(PipelineStage::Completing) => RunStatus::Delivered,
                    Some(PipelineStage::Executing) => RunStatus::InProgress,
                    _ => initial_status,
                };
                let past_planning = matches!(
                    stage,
                    Some(PipelineStage::Executing) | Some(PipelineStage::Completing)
                );
                if let Some(run) = self.state.runs.get_mut(&existing_id) {
                    run.status = reuse_status;
                    run.manual_dispatch_directives = manual_dispatch_directives.clone();
                    run.updated_at = Utc::now();
                    if !past_planning {
                        run.pipeline_stage = if has_planner {
                            Some(PipelineStage::Planning)
                        } else {
                            Some(PipelineStage::Executing)
                        };
                    }
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                info!(
                    issue_identifier = %issue.identifier,
                    run_id = %existing_id,
                    workspace_path = %workspace.path.display(),
                    has_planner,
                    existing_stage = ?stage,
                    reuse_status = ?reuse_status,
                    "pipeline run reused"
                );
                (existing_id, stage)
            } else {
                let run_id = new_run_id();
                let now = Utc::now();
                let pipeline_stage = if has_planner {
                    Some(PipelineStage::Planning)
                } else {
                    Some(PipelineStage::Executing)
                };
                let initial_steps = if has_planner {
                    polyphony_core::build_planner_steps()
                } else {
                    Vec::new()
                };
                let run = Run {
                    id: run_id.clone(),
                    kind: RunKind::IssueDelivery,
                    issue_id: Some(issue.id.clone()),
                    issue_identifier: Some(issue.identifier.clone()),
                    title: issue.title.clone(),
                    status: initial_status,
                    pipeline_stage,
                    manual_dispatch_directives: manual_dispatch_directives.clone(),
                    workspace_key: Some(sanitize_workspace_key(&issue.identifier)),
                    workspace_path: Some(workspace.path.clone()),
                    review_target: None,
                    deliverable: None,
                    created_at: now,
                    updated_at: now,
                    cancel_reason: None,
                    blocked_outcome: None,
                    steps: initial_steps,
                    activity_log: Vec::new(),
                };
                if let Some(store) = &self.store {
                    store.save_run(&run).await?;
                }
                self.state.runs.insert(run_id.clone(), run);
                info!(
                    issue_identifier = %issue.identifier,
                    run_id,
                    workspace_path = %workspace.path.display(),
                    has_planner,
                    initial_status = ?initial_status,
                    "pipeline run created"
                );
                (run_id, None)
            };

        // Pipeline already completed — check for deliverable, mark failed if no output.
        if matches!(existing_stage, Some(PipelineStage::Completing)) {
            let has_deliverable = self
                .state
                .runs
                .get(&run_id)
                .is_some_and(|m| m.deliverable.is_some());
            if !has_deliverable {
                // Try to detect any changes in the workspace
                self.create_local_branch_deliverable_from_workspace(&run_id, &workspace.path)
                    .await;
            }
            // Check if the deliverable has actual changes
            let deliverable = self
                .state
                .runs
                .get(&run_id)
                .and_then(|m| m.deliverable.as_ref());
            let confirmed_no_changes = deliverable.is_some_and(|d| {
                d.metadata
                    .get("lines_added")
                    .and_then(|v| v.as_u64())
                    .is_some_and(|added| added == 0)
            });
            let no_output = confirmed_no_changes || deliverable.is_none();
            if no_output {
                warn!(
                    issue_identifier = %issue.identifier,
                    run_id,
                    "pipeline completed with no code changes — marking as failed"
                );
                if let Some(run) = self.state.runs.get_mut(&run_id) {
                    run.status = RunStatus::Failed;
                    run.updated_at = Utc::now();
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "{} pipeline failed: completed without producing any code changes",
                        issue.identifier
                    ),
                );
            } else {
                info!(
                    issue_identifier = %issue.identifier,
                    run_id,
                    "pipeline already completed, skipping re-dispatch"
                );
            }
            return Ok(());
        }

        // Tasks exist from a prior planner run — skip the router and resume.
        if matches!(existing_stage, Some(PipelineStage::Executing)) {
            info!(
                issue_identifier = %issue.identifier,
                run_id,
                "skipping planner — resuming from next pending task"
            );
            return self
                .dispatch_next_task(
                    workflow,
                    issue,
                    attempt,
                    prefer_alternate_agent,
                    &run_id,
                    &workspace.path,
                )
                .await;
        }

        if has_planner {
            self.dispatch_planner_task(
                &workflow,
                &issue,
                attempt,
                prefer_alternate_agent,
                &run_id,
                &workspace.path,
            )
            .await
        } else {
            let tasks = self.create_tasks_from_stages(&workflow.config.pipeline.stages, &run_id);
            if let Some(store) = &self.store {
                for task in &tasks {
                    store.save_task(task).await?;
                }
            }
            info!(
                issue_identifier = %issue.identifier,
                run_id,
                stage_tasks = tasks.len(),
                "pipeline stages expanded without planner"
            );
            // Populate delivery steps from the created tasks.
            let task_ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
            if let Some(run) = self.state.runs.get_mut(&run_id) {
                run.steps = polyphony_core::build_delivery_steps(
                    &task_ids,
                    workflow.config.automation.enabled,
                    workflow.config.pr_review_agent().ok().flatten().is_some(),
                );
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            self.state.tasks.insert(run_id.clone(), tasks);
            self.dispatch_next_task(
                workflow,
                issue,
                attempt,
                prefer_alternate_agent,
                &run_id,
                &workspace.path,
            )
            .await
        }
    }

    pub(crate) fn create_tasks_from_stages(
        &self,
        stages: &[polyphony_workflow::PipelineStageConfig],
        run_id: &str,
    ) -> Vec<Task> {
        let now = Utc::now();
        stages
            .iter()
            .enumerate()
            .map(|(index, stage)| {
                let category = match stage.category.to_ascii_lowercase().as_str() {
                    "research" => polyphony_core::TaskCategory::Research,
                    "coding" => polyphony_core::TaskCategory::Coding,
                    "testing" => polyphony_core::TaskCategory::Testing,
                    "documentation" => polyphony_core::TaskCategory::Documentation,
                    "review" => polyphony_core::TaskCategory::Review,
                    _ => polyphony_core::TaskCategory::Coding,
                };
                Task {
                    id: format!("task-{}", uuid::Uuid::new_v4()),
                    run_id: run_id.to_string(),
                    title: format!("{} stage", stage.category),
                    description: None,
                    activity_log: Vec::new(),
                    category,
                    role: stage.role,
                    status: TaskStatus::Pending,
                    ordinal: (index + 1) as u32,
                    parent_id: None,
                    agent_name: stage.agent.clone(),
                    session_id: None,
                    thread_id: None,
                    turns_completed: 0,
                    tokens: TokenUsage::default(),
                    started_at: None,
                    finished_at: None,
                    error: None,
                    created_at: now,
                    updated_at: now,
                }
            })
            .collect()
    }

    pub(crate) async fn dispatch_planner_task(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        attempt: Option<u32>,
        _prefer_alternate_agent: bool,
        run_id: &str,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        let planner_agent_name = workflow.config.router_agent_name().ok_or_else(|| {
            Error::Core(CoreError::Adapter(
                "orchestration.router_agent or pipeline.planner_agent is required".into(),
            ))
        })?;
        let profile = workflow
            .config
            .agents
            .profiles
            .get(planner_agent_name)
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "unknown planner agent `{planner_agent_name}`"
                )))
            })?;
        let selected_agent = agent_definition_with_pty(
            planner_agent_name,
            profile,
            workflow.config.agent.pty_backend,
        );
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            planner_agent = %selected_agent.name,
            attempt = attempt.unwrap_or(0),
            "dispatching pipeline planner"
        );

        let prompt = workflow
            .config
            .pipeline
            .planner_prompt
            .as_deref()
            .map(|template| render_issue_template_with_strings(template, issue, attempt, &[]))
            .unwrap_or_else(|| {
                render_issue_template_with_strings(DEFAULT_PLANNER_PROMPT, issue, attempt, &[])
            })?;
        let prompt = apply_agent_prompt_template(
            workflow,
            &selected_agent.name,
            prompt,
            issue,
            attempt,
            1,
            workflow.config.agent.max_turns,
        )?;
        let prompt =
            prepend_manual_dispatch_directives(prompt, self.manual_dispatch_directives(run_id));

        // Mark the PlannerRun step as running.
        if let Some(run) = self.state.runs.get_mut(run_id)
            && let Some(step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == polyphony_core::StepKind::PlannerRun && !s.is_complete())
        {
            step.mark_running();
        }

        self.spawn_pipeline_worker(
            workflow.clone(),
            issue.clone(),
            attempt,
            workspace_path.to_path_buf(),
            prompt,
            selected_agent,
            None,
            Some(run_id.to_string()),
            None,
        )
        .await
    }

    pub(crate) async fn dispatch_next_task(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        _prefer_alternate_agent: bool,
        run_id: &str,
        workspace_path: &Path,
    ) -> Result<(), Error> {
        if self
            .state
            .runs
            .get(run_id)
            .is_some_and(|run| run.status == RunStatus::Blocked)
        {
            self.push_event(
                EventScope::Dispatch,
                format!(
                    "{} pipeline continuation skipped: run is blocked",
                    issue.identifier
                ),
            );
            return Ok(());
        }
        // A restored run can contain a Git commit made immediately before a
        // crash but no durable outcome. Reconcile every completed mutating
        // stage before any later agent, especially QA, is allowed to start.
        if workflow.config.local_commit.enabled {
            let completed_mutating_tasks = self
                .state
                .tasks
                .get(run_id)
                .map(|tasks| {
                    tasks
                        .iter()
                        .filter(|task| {
                            task.status == TaskStatus::Completed
                                && matches!(
                                    task.role,
                                    polyphony_core::PipelineTaskRole::Implementation
                                        | polyphony_core::PipelineTaskRole::Repair
                                )
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for task in completed_mutating_tasks {
                self.commit_local_stage(&workflow, &issue, run_id, &task, workspace_path)
                    .await?;
            }
        }
        let next_task = self.state.tasks.get(run_id).and_then(|tasks| {
            tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Pending)
                .min_by_key(|task| task.ordinal)
                .cloned()
        });

        let Some(task) = next_task else {
            self.complete_pipeline(&workflow, &issue, run_id).await?;
            return Ok(());
        };

        // Select agent for this task
        let agent_name = task
            .agent_name
            .clone()
            .or_else(|| {
                workflow
                    .config
                    .pipeline
                    .stages
                    .iter()
                    .find(|s| s.category.eq_ignore_ascii_case(&task.category.to_string()))
                    .and_then(|s| s.agent.clone())
            })
            .or_else(|| workflow.config.agents.default.clone())
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(
                    "no agent available for pipeline task".into(),
                ))
            })?;

        let profile = workflow
            .config
            .agents
            .profiles
            .get(&agent_name)
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "unknown agent `{agent_name}` for pipeline task"
                )))
            })?;
        if workflow.config.local_commit.enabled {
            let required_sandbox = match task.role {
                polyphony_core::PipelineTaskRole::Implementation
                | polyphony_core::PipelineTaskRole::Repair => "workspace-write",
                polyphony_core::PipelineTaskRole::Qa => "read-only",
            };
            if profile.thread_sandbox.as_deref() != Some(required_sandbox)
                || profile.turn_sandbox_policy.as_deref() != Some(required_sandbox)
            {
                return Err(Error::Core(CoreError::Adapter(format!(
                    "local_commit requires `{required_sandbox}` thread and turn sandboxes for agent `{agent_name}`"
                ))));
            }
        }
        let selected_agent =
            agent_definition_with_pty(&agent_name, profile, workflow.config.agent.pty_backend);
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            task_id = %task.id,
            task_title = %task.title,
            task_category = %task.category,
            task_ordinal = task.ordinal,
            task_count = self.state.tasks.get(run_id).map(|tasks| tasks.len()).unwrap_or(0),
            selected_agent = %selected_agent.name,
            "dispatching next pipeline task"
        );

        // Build task prompt with pipeline context
        let prompt = self.build_task_prompt(
            &workflow,
            &selected_agent.name,
            &issue,
            &task,
            run_id,
            attempt,
            workflow.config.agent.max_turns,
        )?;

        // Mark task in progress
        if let Some(tasks) = self.state.tasks.get_mut(run_id)
            && let Some(t) = tasks.iter_mut().find(|t| t.id == task.id)
        {
            t.status = TaskStatus::InProgress;
            t.started_at = Some(Utc::now());
            t.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_task(t).await?;
            }
        }

        // Build prior context from the task's stored session info for resume.
        let prior_context = if task.session_id.is_some() || task.thread_id.is_some() {
            Some(AgentContextSnapshot {
                repo_id: self.repo_id_for_issue(&issue.id),
                issue_id: issue.id.clone(),
                issue_identifier: issue.identifier.clone(),
                updated_at: Utc::now(),
                agent_name: selected_agent.name.clone(),
                model: selected_agent.model.clone(),
                session_id: task.session_id.clone(),
                thread_id: task.thread_id.clone(),
                turn_id: None,
                codex_app_server_pid: None,
                status: None,
                error: None,
                usage: TokenUsage::default(),
                transcript: Vec::new(),
            })
        } else {
            None
        };

        // Mark the AgentRun step as running.
        if let Some(run) = self.state.runs.get_mut(run_id)
            && let Some(step) = run.steps.iter_mut().find(|s| {
                s.kind == polyphony_core::StepKind::AgentRun
                    && s.task_id.as_deref() == Some(&task.id)
                    && !s.is_complete()
            })
        {
            step.mark_running();
        }

        self.spawn_pipeline_worker(
            workflow,
            issue,
            attempt,
            workspace_path.to_path_buf(),
            prompt,
            selected_agent,
            Some(task.id.clone()),
            Some(run_id.to_string()),
            prior_context,
        )
        .await
    }

    pub(crate) fn build_task_prompt(
        &self,
        workflow: &LoadedWorkflow,
        agent_name: &str,
        issue: &Issue,
        task: &Task,
        run_id: &str,
        attempt: Option<u32>,
        max_turns: u32,
    ) -> Result<String, Error> {
        let tasks = self.state.tasks.get(run_id);
        let completed_tasks: Vec<String> = tasks
            .map(|ts| {
                ts.iter()
                    .filter(|t| t.status == TaskStatus::Completed)
                    .map(|t| {
                        format!(
                            "- [{}] {}: {}",
                            t.category,
                            t.title,
                            t.description.as_deref().unwrap_or("completed")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let total_tasks = tasks.map(|ts| ts.len()).unwrap_or(0);

        // Read plan.json if it exists
        let has_plan = self
            .state
            .runs
            .get(run_id)
            .and_then(|m| m.workspace_path.as_ref())
            .is_some_and(|path| path.join(".polyphony").join("plan.json").exists());

        // Render the workflow template with pipeline context injected
        let base_prompt = apply_agent_prompt_template(
            workflow,
            agent_name,
            render_turn_prompt(&workflow.definition, issue, attempt, 1, max_turns)?,
            issue,
            attempt,
            1,
            max_turns,
        )?;

        let mut prompt = prepend_manual_dispatch_directives(
            base_prompt,
            self.manual_dispatch_directives(run_id),
        );
        prompt.push_str(&format!(
            "\n\n## Pipeline Task {}/{}\n\
             **Task:** {}\n\
             **Category:** {}\n\
             **Role:** {}\n",
            task.ordinal, total_tasks, task.title, task.category, task.role
        ));
        if task.role == polyphony_core::PipelineTaskRole::Qa {
            prompt.push_str(
                "\nYou are the independent QA role. Do not modify the workspace, run git mutation, \
                 commit, push, create a pull request, or dispatch repair work. Inspect and test only; \
                 publish a durable PASS or FAIL verdict with evidence through the tracker evidence tool. \
                 Your final evidence must use `QA PASS:` or `QA FAIL:` followed by nonempty `tests run:` \
                 and `checks:` lines. The checks line must identify every acceptance check. For `QA FAIL:`, \
                 also use exactly one line each for `realistic: yes|no`, `material: yes|no`, \
                 `risks: none|false pass, lost evidence, duplicate work, human-control bypass`, \
                 `small fix: yes|no`, and `recommendation: remediate|defer|needs human decision`. \
                 A deferred recommendation also requires `follow-up: <issue or tracker reference>`. Your durable \
                 verdict is your explicit stage-completion signal: after it succeeds, stop working and let Polyphony \
                 perform the handoff. Do not request a tracker-state change.\n",
            );
        } else if task.role == polyphony_core::PipelineTaskRole::Implementation {
            prompt.push_str(
                "\nBefore completing, publish `IMPLEMENTATION NOTE:` with exactly one nonempty checklist line for each: \
                 `what changed:`, `commit:` (use `none — <reason>` if no commit exists), `tests run:`, \
                 and `checks:`. That durable note is your explicit stage-completion signal: after it succeeds, \
                 stop working and let Polyphony perform the handoff. Do not wait for or request a tracker-state change.\n",
            );
        } else if task.role == polyphony_core::PipelineTaskRole::Repair {
            prompt.push_str(
                "\nBefore completing, publish `REPAIR NOTE:` with exactly one nonempty checklist line for each: \
                 `what fixed:`, `commit:` (use `none — <reason>` if no commit exists), `tests run:`, \
                 `recheck:`, and `checks:`. That durable note is your explicit stage-completion signal: after it \
                 succeeds, stop working and let Polyphony perform the handoff. Do not wait for or request a \
                 tracker-state change.\n",
            );
        }
        if let Some(desc) = &task.description {
            prompt.push_str(&format!("**Description:** {desc}\n"));
        }
        if !completed_tasks.is_empty() {
            prompt.push_str("\n### Completed tasks\n");
            prompt.push_str(&completed_tasks.join("\n"));
            prompt.push('\n');
        }
        if has_plan {
            prompt.push_str("\n### Execution plan\nThe full plan is in `.polyphony/plan.json`.\n");
        }
        prompt.push_str(
            "\nRead any workspace artifacts from previous tasks in `.polyphony/` for context.\n",
        );

        Ok(prompt)
    }

    fn manual_dispatch_directives(&self, run_id: &str) -> Option<&str> {
        self.state
            .runs
            .get(run_id)
            .and_then(|run| run.manual_dispatch_directives.as_deref())
    }

    pub(crate) async fn spawn_pipeline_worker(
        &mut self,
        workflow: LoadedWorkflow,
        issue: Issue,
        attempt: Option<u32>,
        workspace_path: PathBuf,
        prompt: String,
        selected_agent: polyphony_core::AgentDefinition,
        active_task_id: Option<TaskId>,
        run_id: Option<RunId>,
        prior_context: Option<AgentContextSnapshot>,
    ) -> Result<(), Error> {
        let issue_id = issue.id.clone();
        let issue_identifier = issue.identifier.clone();
        let issue_identifier_for_task = issue_identifier.clone();
        let issue_for_task = issue.clone();
        let command_tx = self.command_tx.clone();
        let agent = self.agent_for_issue(&issue.id);
        let tracker = self.tracker_for_issue(&issue.id);
        let provisioner = self.provisioner.clone();
        let hooks = workflow.config.hooks.clone();
        let active_states = workflow.config.tracker.active_states.clone();
        let max_turns = workflow.config.agent.max_turns;
        let completion_signal = active_task_id.as_ref().and_then(|task_id| {
            run_id.as_ref().and_then(|run_id| {
                self.state
                    .tasks
                    .get(run_id)
                    .and_then(|tasks| tasks.iter().find(|task| task.id == *task_id))
                    .map(|task| StageCompletionSignal::for_role(task.role))
            })
        });
        let started_at = Utc::now();
        let (stop_tx, stop_rx) = watch::channel(None);
        let selected_agent_for_task = selected_agent.clone();
        let workspace_path_for_running = workspace_path.clone();

        info!(
            issue_identifier = %issue.identifier,
            agent = %selected_agent.name,
            task_id = ?active_task_id,
            run_id = ?run_id,
            "dispatching pipeline worker"
        );

        let worker_span = info_span!(
            "pipeline_worker",
            issue_identifier = %issue_identifier_for_task,
            agent = %selected_agent_for_task.name,
        );
        let handle = tokio::spawn(
            async move {
                let manager = WorkspaceManager::new(
                    workflow.config.workspace.root.clone(),
                    provisioner,
                    workflow.config.workspace.checkout_kind,
                    workflow.config.workspace.sync_on_reuse,
                    workflow.config.workspace.transient_paths.clone(),
                    workflow.config.workspace.source_repo_path.clone(),
                    workflow.config.workspace.clone_url.clone(),
                    workflow.config.workspace.default_branch.clone(),
                );
                let outcome = run_worker_attempt(
                    &manager,
                    &hooks,
                    agent,
                    tracker,
                    issue_for_task,
                    attempt,
                    workspace_path.clone(),
                    prompt,
                    active_states,
                    max_turns,
                    workflow.config.agent.continuation_prompt.clone(),
                    completion_signal,
                    selected_agent_for_task,
                    prior_context,
                    stop_rx,
                    command_tx.clone(),
                )
                .await;
                let outcome = match outcome {
                    Ok(result) => result,
                    Err(error) => agent_run_result_from_error(&error),
                };
                let _ = command_tx.send(OrchestratorMessage::WorkerFinished {
                    issue_id,
                    issue_identifier: issue_identifier_for_task,
                    attempt,
                    started_at,
                    outcome,
                });
            }
            .instrument(worker_span),
        );

        self.claim_issue(issue.id.clone(), IssueClaimState::Running);
        self.state.retrying.remove(&issue.id);
        self.state.running.insert(issue.id.clone(), RunningTask {
            issue,
            agent_name: selected_agent.name.clone(),
            model: selected_agent
                .model
                .clone()
                .or_else(|| {
                    self.state
                        .agent_catalogs
                        .get(&selected_agent.name)
                        .and_then(|catalog| catalog.selected_model.clone())
                })
                .or_else(|| selected_agent.models.first().cloned()),
            attempt,
            workspace_path: workspace_path_for_running,
            stall_timeout_ms: selected_agent.stall_timeout_ms,
            max_turns,
            started_at,
            session_id: None,
            thread_id: None,
            turn_id: None,
            codex_app_server_pid: None,
            last_event: Some("pipeline_dispatch_started".into()),
            last_message: Some("pipeline worker launched".into()),
            last_event_at: Some(Utc::now()),
            tokens: TokenUsage::default(),
            last_reported_tokens: TokenUsage::default(),
            turn_count: 0,
            rate_limits: None,
            stop_tx,
            handle,
            active_task_id,
            run_id,
            review_target: None,
            review_comment_marker: None,
            recent_log: VecDeque::new(),
        });
        self.push_event(
            EventScope::Dispatch,
            format!("pipeline dispatched {issue_identifier}"),
        );
        Ok(())
    }

    pub(crate) async fn handle_planner_finished(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        run_id: &str,
        workspace_path: &Path,
        outcome: &AgentRunResult,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        if matches!(
            outcome.status,
            AttemptStatus::CancelledByReconciliation | AttemptStatus::CancelledByUser
        ) {
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Cancelled;
                run.cancel_reason = outcome.error.clone();
                for step in &mut run.steps {
                    // Cancellation is terminal for the whole pipeline. Keep
                    // no pending/running steps that restart recovery could
                    // reinterpret as work to resume.
                    if !matches!(step.status, polyphony_core::StepStatus::Succeeded) {
                        step.mark_skipped();
                    }
                }
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            self.push_event(
                EventScope::Dispatch,
                format!("{} pipeline planner cancelled", issue.identifier),
            );
            return Ok(());
        }

        if !matches!(outcome.status, AttemptStatus::Succeeded) {
            warn!(
                issue_identifier = %issue.identifier,
                run_id,
                "planner failed, marking run as failed"
            );
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            return Ok(());
        }

        let plan_path = workspace_path.join(".polyphony").join("plan.json");
        let plan_raw = tokio::fs::read_to_string(&plan_path)
            .await
            .map_err(|error| {
                Error::Core(CoreError::Adapter(format!(
                    "failed to read plan.json: {error}"
                )))
            })?;
        let plan: PipelinePlan = serde_json::from_str(&plan_raw).map_err(|error| {
            Error::Core(CoreError::Adapter(format!(
                "failed to parse plan.json: {error}"
            )))
        })?;
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            plan_path = %plan_path.display(),
            planned_tasks = plan.tasks.len(),
            "planner output loaded"
        );

        if plan.tasks.is_empty() {
            warn!(
                issue_identifier = %issue.identifier,
                "planner produced empty plan"
            );
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            return Ok(());
        }

        let planned_tasks = plan
            .tasks
            .iter()
            .cloned()
            .map(|mut planned_task| {
                if let Some(agent_name) = &planned_task.agent
                    && !workflow.config.agents.profiles.contains_key(agent_name)
                {
                    warn!(
                        issue_identifier = %issue.identifier,
                        agent = agent_name,
                        "planner referenced unknown agent, ignoring agent hint"
                    );
                    planned_task.agent = None;
                }
                planned_task
            })
            .collect::<Vec<_>>();

        let tasks: Vec<Task> = planned_tasks
            .iter()
            .enumerate()
            .map(|(index, planned)| planned.to_task(run_id, (index + 1) as u32))
            .collect();

        if let Some(store) = &self.store {
            for task in &tasks {
                store.save_task(task).await?;
            }
        }
        for task in &tasks {
            info!(
                issue_identifier = %issue.identifier,
                run_id,
                task_id = %task.id,
                task_title = %task.title,
                task_category = %task.category,
                task_ordinal = task.ordinal,
                assigned_agent = task.agent_name.as_deref().unwrap_or("auto"),
                "planner task registered"
            );
        }
        // Build delivery steps from the tasks the planner created.
        let task_ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        self.state.tasks.insert(run_id.to_string(), tasks);

        if let Some(run) = self.state.runs.get_mut(run_id) {
            // Mark the PlannerRun step as succeeded.
            if let Some(planner_step) = run
                .steps
                .iter_mut()
                .find(|s| s.kind == polyphony_core::StepKind::PlannerRun)
            {
                planner_step.mark_succeeded();
            }
            // Append the delivery steps (AgentRun per task + handoff steps).
            let next_ordinal = run.steps.last().map(|s| s.ordinal + 1).unwrap_or(0);
            let mut delivery_steps = polyphony_core::build_delivery_steps(
                &task_ids,
                workflow.config.automation.enabled,
                workflow.config.pr_review_agent().ok().flatten().is_some(),
            );
            for (i, step) in delivery_steps.iter_mut().enumerate() {
                step.ordinal = next_ordinal + i as u32;
            }
            run.steps.extend(delivery_steps);

            run.status = RunStatus::InProgress;
            run.pipeline_stage = Some(PipelineStage::Executing);
            run.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!(
                    "planning → executing: {} tasks created",
                    self.state.tasks.get(run_id).map(|t| t.len()).unwrap_or(0)
                ),
            );
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }

        self.push_event(
            EventScope::Dispatch,
            format!(
                "{} planner created {} tasks",
                issue.identifier,
                self.state.tasks.get(run_id).map(|t| t.len()).unwrap_or(0)
            ),
        );

        self.dispatch_next_task(
            self.workflow(),
            issue.clone(),
            attempt,
            false,
            run_id,
            workspace_path,
        )
        .await
    }

    pub(crate) async fn handle_task_finished(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        run_id: &str,
        task_id: &str,
        workspace_path: &Path,
        outcome: &AgentRunResult,
        attempt: Option<u32>,
    ) -> Result<(), Error> {
        let now = Utc::now();
        let task_snapshot = self
            .state
            .tasks
            .get(run_id)
            .and_then(|tasks| tasks.iter().find(|t| t.id == task_id))
            .cloned();
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            task_id,
            task_title = task_snapshot.as_ref().map(|task| task.title.as_str()).unwrap_or("unknown"),
            task_category = task_snapshot.as_ref().map(|task| task.category.to_string()).unwrap_or_else(|| "unknown".into()),
            status = ?outcome.status,
            turns_completed = outcome.turns_completed,
            error = outcome.error.as_deref().unwrap_or("none"),
            "pipeline task finished"
        );
        let is_qa = task_snapshot
            .as_ref()
            .is_some_and(|task| task.role == polyphony_core::PipelineTaskRole::Qa);
        let qa_result = if is_qa && matches!(outcome.status, AttemptStatus::Succeeded) {
            Some(Self::qa_verdict(outcome))
        } else {
            None
        };
        let evidence_required = is_qa
            || workflow
                .config
                .pipeline
                .stages
                .iter()
                .any(|stage| stage.role == polyphony_core::PipelineTaskRole::Qa);
        let delivery_note = task_snapshot.as_ref().and_then(|task| {
            (evidence_required && matches!(outcome.status, AttemptStatus::Succeeded))
                .then(|| Self::delivery_note(task, issue, outcome))
        });
        let mut evidence_error = delivery_note
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .cloned();
        let mut quality_bar = None;
        if is_qa
            && evidence_error.is_none()
            && let Some(Ok((false, evidence))) = qa_result.as_ref()
        {
            match Self::qa_quality_bar_assessment(evidence).and_then(|mut assessment| {
                assessment.human_override = Self::quality_bar_override(issue)?;
                Ok(assessment)
            }) {
                Ok(assessment) => quality_bar = Some(assessment),
                Err(error) => evidence_error = Some(error),
            }
        }
        let qa_failed = is_qa
            && (!matches!(outcome.status, AttemptStatus::Succeeded)
                || qa_result
                    .as_ref()
                    .is_some_and(|result| result.as_ref().is_ok_and(|(passed, _)| !passed))
                || qa_result.as_ref().is_some_and(Result::is_err)
                || evidence_error.is_some());

        if let (Some(task), Some(Ok(note))) = (task_snapshot.as_ref(), delivery_note.as_ref()) {
            self.publish_delivery_note(issue, run_id, task, note)
                .await?;
        }

        if let Some(tasks) = self.state.tasks.get_mut(run_id)
            && let Some(task) = tasks.iter_mut().find(|t| t.id == task_id)
        {
            task.status = if evidence_error.is_some() {
                TaskStatus::Failed
            } else if is_qa {
                if qa_failed {
                    TaskStatus::Failed
                } else {
                    TaskStatus::Completed
                }
            } else {
                match outcome.status {
                    AttemptStatus::Succeeded => TaskStatus::Completed,
                    AttemptStatus::CancelledByReconciliation | AttemptStatus::CancelledByUser => {
                        TaskStatus::Cancelled
                    },
                    _ => TaskStatus::Failed,
                }
            };
            task.turns_completed = outcome.turns_completed;
            task.error = if let Some(error) = evidence_error.as_ref() {
                Some(error.clone())
            } else if is_qa {
                match qa_result.as_ref() {
                    Some(Ok((false, evidence))) => Some(format!("QA FAIL: {evidence}")),
                    Some(Err(error)) => Some(error.clone()),
                    _ => outcome.error.clone(),
                }
            } else {
                outcome.error.clone()
            };
            if let Some(Ok((passed, evidence))) = qa_result.as_ref() {
                task.activity_log.push(format!(
                    "durable QA {} evidence: {}",
                    if *passed {
                        "PASS"
                    } else {
                        "FAIL"
                    },
                    evidence
                ));
            }
            task.finished_at = Some(now);
            task.updated_at = now;
            if let Some(store) = &self.store {
                store.save_task(task).await?;
            }
        }

        // Mark the corresponding AgentRun step.
        if let Some(run) = self.state.runs.get_mut(run_id) {
            if let Some(step) = run.steps.iter_mut().find(|s| {
                s.kind == polyphony_core::StepKind::AgentRun
                    && s.task_id.as_deref() == Some(task_id)
            }) {
                if matches!(outcome.status, AttemptStatus::Succeeded)
                    && !qa_failed
                    && evidence_error.is_none()
                {
                    step.mark_succeeded();
                } else if matches!(
                    outcome.status,
                    AttemptStatus::CancelledByReconciliation | AttemptStatus::CancelledByUser
                ) {
                    // Step records intentionally have no cancelled state; a
                    // skipped step preserves that it did not fail and prevents
                    // cancellation from feeding failure/replan logic.
                    step.mark_skipped();
                } else {
                    step.mark_failed(outcome.error.as_deref().unwrap_or("task failed"));
                }
            }
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }

        if qa_failed {
            let Some(quality_bar) = quality_bar else {
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.status = RunStatus::Failed;
                    run.push_log(
                        polyphony_core::RunLogScope::Pipeline,
                        "QA failed without a valid durable quality-bar assessment; repair was not dispatched",
                    );
                    run.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                return Ok(());
            };
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.push_log(
                    polyphony_core::RunLogScope::Pipeline,
                    quality_bar.durable_record(),
                );
                run.updated_at = now;
                // The audit record must be stored before a repair worker can
                // run. A storage error exits this handler and leaves repair
                // pending, which is the fail-closed boundary.
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            match quality_bar.decision() {
                QualityBarDecision::Defer => {
                    self.defer_quality_bar_follow_up(run_id, task_id, &quality_bar)
                        .await?;
                    return self
                        .complete_pipeline(&self.workflow(), issue, run_id)
                        .await;
                },
                QualityBarDecision::NeedsHumanDecision => {
                    if let Some(run) = self.state.runs.get_mut(run_id) {
                        run.status = RunStatus::Review;
                        run.push_log(
                            polyphony_core::RunLogScope::Pipeline,
                            "quality-bar review needs a human decision; repair was not dispatched",
                        );
                        run.updated_at = now;
                        if let Some(store) = &self.store {
                            store.save_run(run).await?;
                        }
                    }
                    return Ok(());
                },
                QualityBarDecision::Remediate => {},
            }
            if let Err(error) = self
                .tracker_for_issue(&issue.id)
                .update_issue_workflow_status(issue, "Repair Needed")
                .await
            {
                // A repair worker must never be released until the failed QA
                // result is durably visible in the tracker.  Otherwise a
                // restart/person could see stale tracker state while code is
                // being changed in response to an invisible failure.
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.status = RunStatus::Failed;
                    run.push_log(
                        polyphony_core::RunLogScope::Pipeline,
                        format!("QA failed, but tracker transition to Repair Needed failed; repair was not dispatched: {error}"),
                    );
                    run.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                return Ok(());
            }
            let repair_is_next = self
                .state
                .tasks
                .get(run_id)
                .and_then(|tasks| {
                    tasks
                        .iter()
                        .filter(|task| task.status == TaskStatus::Pending)
                        .min_by_key(|task| task.ordinal)
                        .map(|task| task.role == polyphony_core::PipelineTaskRole::Repair)
                })
                .unwrap_or(false);
            if repair_is_next {
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.push_log(
                        polyphony_core::RunLogScope::Pipeline,
                        format!("QA failed; dispatching the next distinct repair role for task {task_id}"),
                    );
                    run.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                return self
                    .dispatch_next_task(
                        self.workflow(),
                        issue.clone(),
                        attempt,
                        false,
                        run_id,
                        workspace_path,
                    )
                    .await;
            }
            let completed_repairs = self
                .state
                .tasks
                .get(run_id)
                .map(|tasks| {
                    tasks
                        .iter()
                        .filter(|task| {
                            task.role == polyphony_core::PipelineTaskRole::Repair
                                && task.status == TaskStatus::Completed
                        })
                        .count()
                })
                .unwrap_or_default();
            if completed_repairs >= 2 {
                if let Err(error) = self
                    .tracker_for_issue(&issue.id)
                    .update_issue_workflow_status(issue, "Needs Human Decision")
                    .await
                {
                    if let Some(run) = self.state.runs.get_mut(run_id) {
                        run.status = RunStatus::Failed;
                        run.push_log(
                            polyphony_core::RunLogScope::Pipeline,
                            format!("QA failed after {completed_repairs} repairs, but tracker transition to Needs Human Decision failed: {error}"),
                        );
                        run.updated_at = now;
                        if let Some(store) = &self.store {
                            store.save_run(run).await?;
                        }
                    }
                    return Ok(());
                }
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.status = RunStatus::Review;
                    run.push_log(
                        polyphony_core::RunLogScope::Pipeline,
                        format!("closed-loop repair limit reached after {completed_repairs} repairs; QA evidence is durable and needs human decision"),
                    );
                    run.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
            } else if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.push_log(
                    polyphony_core::RunLogScope::Pipeline,
                    "QA failed without a pending distinct repair task; pipeline stopped",
                );
                run.updated_at = now;
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            return Ok(());
        }

        if let Some(error) = evidence_error {
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.push_log(polyphony_core::RunLogScope::Pipeline, error);
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            return Ok(());
        }

        if is_qa && matches!(qa_result.as_ref(), Some(Ok((true, _)))) {
            let passed_qa_ordinal = task_snapshot
                .as_ref()
                .map(|task| task.ordinal)
                .unwrap_or(u32::MAX);
            if let Some(tasks) = self.state.tasks.get_mut(run_id) {
                for task in tasks.iter_mut().filter(|task| {
                    task.status == TaskStatus::Pending
                        && task.ordinal > passed_qa_ordinal
                        && matches!(
                            task.role,
                            polyphony_core::PipelineTaskRole::Qa
                                | polyphony_core::PipelineTaskRole::Repair
                        )
                }) {
                    task.status = TaskStatus::Cancelled;
                    task.error = Some("QA PASS completed the closed-loop delivery".into());
                    task.finished_at = Some(now);
                    task.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_task(task).await?;
                    }
                }
            }
            if let Some(run) = self.state.runs.get_mut(run_id) {
                for step in &mut run.steps {
                    if step.kind == polyphony_core::StepKind::AgentRun
                        && step.task_id.as_deref().is_some_and(|task_id| {
                            self.state.tasks.get(run_id).is_some_and(|tasks| {
                                tasks.iter().any(|task| {
                                    task.id == task_id && task.status == TaskStatus::Cancelled
                                })
                            })
                        })
                    {
                        step.mark_skipped();
                    }
                }
                run.push_log(
                    polyphony_core::RunLogScope::Pipeline,
                    "closed-loop QA PASS completed delivery; remaining repair and QA stages were cancelled",
                );
                run.updated_at = now;
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            return self
                .complete_pipeline(&self.workflow(), issue, run_id)
                .await;
        }

        if matches!(outcome.status, AttemptStatus::Succeeded) {
            if workflow.config.local_commit.enabled
                && task_snapshot.as_ref().is_some_and(|task| {
                    matches!(
                        task.role,
                        polyphony_core::PipelineTaskRole::Implementation
                            | polyphony_core::PipelineTaskRole::Repair
                    )
                })
                && let Some(task) = task_snapshot.as_ref()
            {
                self.commit_local_stage(workflow, issue, run_id, task, workspace_path)
                    .await?;
            }
            self.dispatch_next_task(
                self.workflow(),
                issue.clone(),
                attempt,
                false,
                run_id,
                workspace_path,
            )
            .await
        } else if matches!(
            outcome.status,
            AttemptStatus::CancelledByReconciliation | AttemptStatus::CancelledByUser
        ) {
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Cancelled;
                run.cancel_reason = outcome.error.clone();
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            Ok(())
        } else {
            let max_replan_attempts = 2;
            if workflow.config.pipeline.replan_on_failure
                && workflow.config.router_agent_name().is_some()
                && attempt.unwrap_or(0) < max_replan_attempts
            {
                self.push_event(
                    EventScope::Dispatch,
                    format!(
                        "{} task failed, re-running planner (attempt {})",
                        issue.identifier,
                        attempt.unwrap_or(0) + 1
                    ),
                );
                // Reset tasks and re-plan
                if let Some(tasks) = self.state.tasks.get_mut(run_id) {
                    for task in tasks.iter_mut() {
                        if task.status == TaskStatus::Pending {
                            task.status = TaskStatus::Cancelled;
                            task.updated_at = Utc::now();
                        }
                    }
                }
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.status = RunStatus::Planning;
                    run.updated_at = Utc::now();
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                return self
                    .dispatch_planner_task(workflow, issue, attempt, false, run_id, workspace_path)
                    .await;
            }
            let repair_ordinal = task_snapshot
                .as_ref()
                .filter(|task| task.role == polyphony_core::PipelineTaskRole::Implementation)
                .and_then(|_| {
                    self.state.tasks.get(run_id).and_then(|tasks| {
                        tasks
                            .iter()
                            .filter(|task| {
                                task.role == polyphony_core::PipelineTaskRole::Repair
                                    && task.status == TaskStatus::Pending
                            })
                            .min_by_key(|task| task.ordinal)
                            .map(|task| task.ordinal)
                    })
                });
            if let Some(repair_ordinal) = repair_ordinal {
                if let Err(error) = self
                    .tracker_for_issue(&issue.id)
                    .update_issue_workflow_status(issue, "Repair Needed")
                    .await
                {
                    if let Some(run) = self.state.runs.get_mut(run_id) {
                        run.status = RunStatus::Failed;
                        run.push_log(
                            polyphony_core::RunLogScope::Pipeline,
                            format!("implementation failed, but tracker transition to Repair Needed failed; repair was not dispatched: {error}"),
                        );
                        run.updated_at = now;
                        if let Some(store) = &self.store {
                            store.save_run(run).await?;
                        }
                    }
                    return Ok(());
                }
                if let Some(tasks) = self.state.tasks.get_mut(run_id) {
                    for task in tasks.iter_mut().filter(|task| {
                        task.status == TaskStatus::Pending
                            && task.ordinal < repair_ordinal
                            && task.role == polyphony_core::PipelineTaskRole::Qa
                    }) {
                        task.status = TaskStatus::Cancelled;
                        task.error = Some("implementation failed; superseded by repair".into());
                        task.finished_at = Some(now);
                        task.updated_at = now;
                        if let Some(store) = &self.store {
                            store.save_task(task).await?;
                        }
                    }
                }
                if let Some(run) = self.state.runs.get_mut(run_id) {
                    run.status = RunStatus::InProgress;
                    run.push_log(
                        polyphony_core::RunLogScope::Pipeline,
                        "implementation failed; skipping stale QA and dispatching the distinct repair role",
                    );
                    run.updated_at = now;
                    if let Some(store) = &self.store {
                        store.save_run(run).await?;
                    }
                }
                return self
                    .dispatch_next_task(
                        self.workflow(),
                        issue.clone(),
                        attempt,
                        false,
                        run_id,
                        workspace_path,
                    )
                    .await;
            }
            // Mark run as failed
            if let Some(run) = self.state.runs.get_mut(run_id) {
                run.status = RunStatus::Failed;
                run.updated_at = Utc::now();
                if let Some(store) = &self.store {
                    store.save_run(run).await?;
                }
            }
            Ok(())
        }
    }

    async fn defer_quality_bar_follow_up(
        &mut self,
        run_id: &str,
        failed_qa_task_id: &str,
        assessment: &QualityBarAssessment,
    ) -> Result<(), Error> {
        let now = Utc::now();
        let follow_up = assessment
            .follow_up
            .as_deref()
            .expect("defer assessment validated a follow-up reference");
        if let Some(tasks) = self.state.tasks.get_mut(run_id) {
            let failed_qa_ordinal = tasks
                .iter()
                .find(|task| task.id == failed_qa_task_id)
                .map(|task| task.ordinal)
                .unwrap_or(u32::MAX);
            for task in tasks.iter_mut().filter(|task| {
                task.status == TaskStatus::Pending
                    && task.ordinal > failed_qa_ordinal
                    && matches!(
                        task.role,
                        polyphony_core::PipelineTaskRole::Qa
                            | polyphony_core::PipelineTaskRole::Repair
                    )
            }) {
                task.status = TaskStatus::Cancelled;
                task.error = Some(format!(
                    "quality-bar deferred as hardening; follow-up: {follow_up}"
                ));
                task.finished_at = Some(now);
                task.updated_at = now;
                if let Some(store) = &self.store {
                    store.save_task(task).await?;
                }
            }
        }
        if let Some(run) = self.state.runs.get_mut(run_id) {
            for step in &mut run.steps {
                if step.kind == polyphony_core::StepKind::AgentRun
                    && step.task_id.as_deref().is_some_and(|task_id| {
                        self.state.tasks.get(run_id).is_some_and(|tasks| {
                            tasks.iter().any(|task| {
                                task.id == task_id && task.status == TaskStatus::Cancelled
                            })
                        })
                    })
                {
                    step.mark_skipped();
                }
            }
            run.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!(
                    "quality-bar deferred non-material hardening after QA task {failed_qa_task_id}; follow-up: {follow_up}; practical acceptance continues"
                ),
            );
            run.updated_at = now;
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn complete_pipeline(
        &mut self,
        workflow: &LoadedWorkflow,
        issue: &Issue,
        run_id: &str,
    ) -> Result<(), Error> {
        let status = if workflow.config.automation.enabled {
            RunStatus::Review
        } else {
            RunStatus::Delivered
        };
        info!(
            issue_identifier = %issue.identifier,
            run_id,
            automation_enabled = workflow.config.automation.enabled,
            run_status = ?status,
            "pipeline completed"
        );
        if let Some(run) = self.state.runs.get_mut(run_id) {
            run.status = status;
            run.pipeline_stage = Some(PipelineStage::Completing);
            run.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!("executing → completing ({status})"),
            );
            run.updated_at = Utc::now();
            if let Some(store) = &self.store {
                store.save_run(run).await?;
            }
        }
        self.push_event(
            EventScope::Dispatch,
            format!("{} pipeline completed ({:?})", issue.identifier, status),
        );
        Ok(())
    }

    pub(crate) async fn inject_feedback_task(
        &mut self,
        request: &FeedbackInjectionRequest,
    ) -> Result<(), Error> {
        let run_id = &request.run_id;
        let Some(run) = self.state.runs.get(run_id).cloned() else {
            return Err(Error::Core(CoreError::Adapter(format!(
                "run {run_id} not found"
            ))));
        };
        if run.status == RunStatus::Blocked {
            return Err(Error::Core(CoreError::Adapter(format!(
                "run {run_id} is blocked and cannot accept continuation feedback"
            ))));
        }
        let Some(issue_id) = &run.issue_id else {
            return Err(Error::Core(CoreError::Adapter(format!(
                "run {run_id} has no associated issue"
            ))));
        };

        // Resolve the issue from tracker_issues
        let issue = self
            .state
            .tracker_issues
            .iter()
            .find(|row| row.issue_id == *issue_id)
            .map(|row| Issue {
                id: row.issue_id.clone(),
                identifier: row.issue_identifier.clone(),
                title: row.title.clone(),
                description: row.description.clone(),
                priority: row.priority,
                state: row.state.clone(),
                labels: row.labels.clone(),
                branch_name: None,
                url: row.url.clone(),
                author: None,
                created_at: row.created_at,
                updated_at: row.updated_at,
                parent_id: row.parent_id.clone(),
                comments: vec![],
                blocked_by: vec![],
                approval_state: row.approval_state,
            })
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!(
                    "issue {issue_id} not found for run {run_id}"
                )))
            })?;

        // Compute next ordinal
        let next_ordinal = self
            .state
            .tasks
            .get(run_id)
            .map(|tasks| tasks.iter().map(|t| t.ordinal).max().unwrap_or(0) + 1)
            .unwrap_or(1);

        // Create the feedback task
        let now = Utc::now();
        let feedback_title = request
            .prompt
            .lines()
            .next()
            .unwrap_or("User feedback")
            .chars()
            .take(80)
            .collect::<String>();
        let task = Task {
            id: format!("task-{}", uuid::Uuid::new_v4()),
            run_id: run_id.clone(),
            title: feedback_title.clone(),
            description: Some(request.prompt.clone()),
            activity_log: Vec::new(),
            category: TaskCategory::Feedback,
            role: polyphony_core::PipelineTaskRole::Implementation,
            status: TaskStatus::Pending,
            ordinal: next_ordinal,
            parent_id: None,
            agent_name: request.agent_name.clone(),
            session_id: None,
            thread_id: None,
            turns_completed: 0,
            tokens: TokenUsage::default(),
            started_at: None,
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        };

        // Persist the task
        if let Some(store) = &self.store {
            store.save_task(&task).await?;
        }
        self.state
            .tasks
            .entry(run_id.clone())
            .or_default()
            .push(task.clone());

        // Add an AgentRun step for this task
        let step_ordinal = run.steps.last().map(|s| s.ordinal + 1).unwrap_or(0);
        let step =
            polyphony_core::StepRecord::new(polyphony_core::StepKind::AgentRun, step_ordinal)
                .with_task_id(task.id.clone());

        if let Some(m) = self.state.runs.get_mut(run_id) {
            m.steps.push(step);
            // Reset run status if delivered/failed so the pipeline resumes
            if matches!(m.status, RunStatus::Delivered | RunStatus::Failed) {
                m.status = RunStatus::InProgress;
                m.pipeline_stage = Some(PipelineStage::Executing);
            }
            // Store user feedback as manual_dispatch_directives for prompt injection
            m.manual_dispatch_directives = Some(request.prompt.clone());
            m.updated_at = now;
            m.push_log(
                polyphony_core::RunLogScope::Pipeline,
                format!("feedback injected: {feedback_title}"),
            );
            if let Some(store) = &self.store {
                store.save_run(m).await?;
            }
        }

        self.push_event(
            EventScope::Dispatch,
            format!(
                "{} feedback task injected: {feedback_title}",
                issue.identifier
            ),
        );

        // Dispatch the next pending task (which will be this feedback task)
        let workflow = self.workflow();
        let workspace_path = run
            .workspace_path
            .as_deref()
            .ok_or_else(|| {
                Error::Core(CoreError::Adapter(format!("run {run_id} has no workspace")))
            })?
            .to_path_buf();

        self.dispatch_next_task(workflow, issue, None, false, run_id, &workspace_path)
            .await?;

        Ok(())
    }
}
