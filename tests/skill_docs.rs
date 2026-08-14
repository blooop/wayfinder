//! Shape checks on the operational snippets the skills ship (#123).
//!
//! The skills under `skills/` are wf's interface: agents copy-paste these
//! snippets verbatim, so their *shape* is behavior. The frontier query in the
//! GitHub tracker doc must be the same single-hop map query the binary issues
//! (`src/fetch.rs`), not a hand-rolled per-child loop — one round trip per
//! frontier read, not one per open child.
//!
//! Offline by design, unlike the `live_*` binaries: the seam is the doc text
//! itself, so no network is involved.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;
use wf::launch::{Aim, Launchable, MapRef};
use wf::model::{Checks, MapId, PrLink, PrStatus, Review, Stage, Ticket, TicketType};

/// The fenced bash snippet under the tracker doc's `## Frontier query`
/// heading — the exact text an agent copies to derive the frontier.
fn frontier_snippet() -> String {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/skills/wf/GITHUB_TRACKER.md"
    ))
    .expect("the tracker doc ships in this repo");
    let section = doc
        .split("## Frontier query")
        .nth(1)
        .expect("the tracker doc documents the frontier query")
        .split("\n## ")
        .next()
        .expect("split always yields a first piece")
        .to_string();
    section
        .split("```bash")
        .nth(1)
        .expect("the frontier query is a fenced, copy-pasteable bash snippet")
        .split("```")
        .next()
        .expect("split always yields a first piece")
        .to_string()
}

/// The frontier costs one round trip (up to the query's `first: 100` cap,
/// same as the binary's) — not a serial REST call per open child.
#[test]
fn frontier_is_one_round_trip() {
    let snippet = frontier_snippet();
    assert_eq!(
        snippet.matches("gh api").count(),
        1,
        "the frontier snippet must issue exactly one gh call:\n{snippet}"
    );
    assert!(
        !snippet.contains("while read"),
        "no per-child loop — the map query already carries every edge:\n{snippet}"
    );
    assert!(
        !snippet.contains("dependencies/blocked_by"),
        "blocking edges come from the map query's blockedBy selection, \
         not a per-child REST endpoint:\n{snippet}"
    );
}

/// The one round trip is the map query the binary itself issues: sub-issues
/// with their `blockedBy` edges in a single GraphQL hop, pinned to the
/// explicit `$REPO` like every other snippet in the doc.
#[test]
fn frontier_mirrors_the_binary_map_query() {
    let snippet = frontier_snippet();
    assert!(
        snippet.contains("gh api graphql"),
        "the frontier is a GraphQL query, same as src/fetch.rs:\n{snippet}"
    );
    for selection in ["subIssues", "blockedBy", "assignees"] {
        assert!(
            snippet.contains(selection),
            "the query must select `{selection}` — the fields the frontier \
             is derived from:\n{snippet}"
        );
    }
    assert!(
        snippet.contains("$REPO") || snippet.contains("${REPO"),
        "every snippet in the doc targets $REPO explicitly:\n{snippet}"
    );
}

/// The manager protocol doc — the handoff contract #126 decided lives here.
fn lifecycle_doc() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/skills/wf/LIFECYCLE.md"
    ))
    .expect("the lifecycle doc ships in this repo")
}

/// The fenced handoff list in the lifecycle doc's step 1 — the exact list of
/// what a manager hands a stage subagent, one entry per line, extraction seam
/// shared with `frontier_snippet()`.
fn handoff_snippet() -> String {
    lifecycle_doc()
        .split("```handoff")
        .nth(1)
        .expect("the lifecycle doc's handoff list is a fenced, extractable block")
        .split("```")
        .next()
        .expect("split always yields a first piece")
        .to_string()
}

/// Every field name a serialized block contains, at any depth — including the
/// tag keys the enums write (`ticket`, `open`). Same walk as the launch
/// module's own `keys_of`; duplicated here because that one is a private test
/// helper.
fn keys_of(value: &Value) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            Value::Object(fields) => {
                for (key, child) in fields {
                    keys.insert(key.clone());
                    stack.push(child);
                }
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    keys
}

/// Everything the manager hands a stage subagent is something a launch's
/// `ctx:` block already carries — pointers, never readings (#126, #134).
///
/// Every identifier in the doc's fenced handoff list must be a field name of a
/// block a **real launch** writes, so the list cannot quietly grow a `ticket_body`
/// entry: documented ⊆ serialized. The two entries with no ctx counterpart —
/// the stage `skill` and the human's `steer` line — are named here explicitly,
/// never waved through, and must themselves appear in the list (they are the
/// whole of what a manager adds beyond the block).
#[test]
fn the_manager_hands_only_what_ctx_carries() {
    let (ticket, map) = documented_example_node();
    let block = launched_block(&ticket, &map, Stage::InReview);
    let serialized = keys_of(
        &serde_json::from_str(&block)
            .unwrap_or_else(|e| panic!("the block is JSON: {block} ({e})")),
    );
    let beyond_ctx = ["skill", "steer"];
    let entries: Vec<String> = handoff_snippet()
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect();
    assert!(
        !entries.is_empty(),
        "the handoff list names what a stage subagent is handed"
    );
    for entry in &entries {
        assert!(
            serialized.contains(entry) || beyond_ctx.contains(&entry.as_str()),
            "`{entry}` is in the documented handoff but no launch serializes it, \
             and it is not one of the two named non-ctx entries {beyond_ctx:?} — \
             the manager hands pointers, never readings"
        );
    }
    for named in beyond_ctx {
        assert!(
            entries.iter().any(|e| e == named),
            "`{named}` is handed beyond the ctx block and the list must own \
             that explicitly"
        );
    }
}

/// The ticket body, its trail, and the map's Decisions-so-far are live reads
/// the stage subagent makes itself — and the manager *names* those reads
/// rather than making them, as copy-pasteable commands pinned to the explicit
/// `$REPO` like every snippet in the bundle (modeled on
/// `frontier_mirrors_the_binary_map_query`).
///
/// The `--comments` flag on the ticket read is behavioral, not cosmetic: a
/// body-only read drops the trail, and trails carry spec amendments written
/// after the manager last read the ticket (#129's amendment to a recorded
/// resolution lived in a breadcrumb).
#[test]
fn the_manager_names_the_reads_it_does_not_make() {
    let doc = lifecycle_doc();
    let ticket_read = r#"gh issue view <n> --repo "$REPO" --comments"#;
    let map_read = r#"gh issue view <map> --repo "$REPO""#;
    assert!(
        doc.contains(ticket_read),
        "the lifecycle doc must name the ticket read verbatim, body and whole \
         trail in one call: {ticket_read}"
    );
    assert!(
        doc.contains(map_read),
        "the lifecycle doc must name the map read verbatim: {map_read}"
    );
}

/// The review stage gets the PR pointer and nothing about the PR — no diff
/// summary, no gate result, no earlier axis report; anything already asserted
/// on the PR is a lead to reproduce, never a finding to carry (#126).
///
/// **This is the weak seam, and deliberately named no stronger than it is:**
/// a phrase check detects doc drift and nothing else. A substring assertion
/// can be satisfied by a sentence that says the opposite (#131's review
/// proved exactly that), and no test in this repo can assert that a manager
/// *obeyed* the sentence — obedience stays unverified, checked only by a
/// human watching a stage subagent's opening tracker calls.
#[test]
fn the_review_stage_is_handed_no_account_of_the_pr() {
    let doc = lifecycle_doc();
    for phrase in [
        "the PR pointer and nothing about the PR",
        "lead to reproduce, never a finding to carry",
    ] {
        assert!(
            doc.contains(phrase),
            "the review-stage paragraph must say {phrase:?}"
        );
    }
}

/// One skill doc's whole text.
fn skill_doc(name: &str) -> String {
    let path = format!("{}/skills/{name}/SKILL.md", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path} ships in this repo: {e}"))
}

/// The tracker doc's `## The launch context (`ctx:`)` section — the shared
/// consumer contract all five skills read (#124).
fn context_section() -> String {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/skills/wf/GITHUB_TRACKER.md"
    ))
    .expect("the tracker doc ships in this repo");
    doc.split("## The launch context")
        .nth(1)
        .expect("the tracker doc documents the launch context block")
        .split("\n## ")
        .next()
        .expect("split always yields a first piece")
        .to_string()
}

/// The prompt a real ticket launch execs — what the docs below describe.
///
/// Built through the binary's own launch path rather than quoted, because
/// these checks exist to bind the docs to the code: the block's prefix, its
/// schema version, its field names and its value vocabularies all have to come
/// from the launch itself, or the docs are only checked against a second copy
/// of themselves.
fn launched_prompt(ticket: &Ticket, map: &MapRef, stage: Stage) -> String {
    let checkout = wf::projects::Checkout::new(
        std::path::PathBuf::from("/data/proj/checkout"),
        ticket.repo.clone(),
    );
    let staged = wf::launch::Staged::ticket(ticket, map, stage).expect("a launchable stage");
    let mode = wf::launch::LaunchMode::picked(
        wf::launch::Agent::Claude,
        wf::launch::Mode::Interactive,
        "",
    );
    let route = staged.route(mode.mode()).expect("a node stop launches");
    match wf::launch::plan(&[checkout], &staged, route, &mode) {
        wf::launch::Targets::One(launch) => launch
            .agent_argv()
            .last()
            .expect("a ticket launch has a prompt")
            .clone(),
        other => panic!("expected one candidate checkout, got {other:?}"),
    }
}

/// The `ctx:` block of that prompt — the JSON text on its own.
fn launched_block(ticket: &Ticket, map: &MapRef, stage: Stage) -> String {
    let prompt = launched_prompt(ticket, map, stage);
    prompt
        .split_once(" ctx: ")
        .unwrap_or_else(|| panic!("a launched skill carries its context: {prompt}"))
        .1
        .to_string()
}

/// The node the tracker doc's worked example describes, field for field.
///
/// The example is the docs' most load-bearing sentence — it is what a skill
/// author reads to learn the field names — so the fixture is built to match it
/// and the launch is asked what it actually writes.
fn documented_example_node() -> (Ticket, MapRef) {
    let ticket = Ticket {
        repo: "owner/name".to_string(),
        number: 124,
        title: "the ticket's title".to_string(),
        status: wf::model::classify(true, false, vec![]),
        ticket_type: TicketType::Build,
        blocked_by: vec![],
        prs: vec![PrLink {
            repo: "owner/name".to_string(),
            number: 130,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::Required,
            },
        }],
    };
    let map = MapRef::new(&MapId::new("owner/name", 121), "the map's title");
    (ticket, map)
}

/// The fenced `json` example under the context section, parsed.
fn documented_example() -> Value {
    let section = context_section();
    let fenced = section
        .split("```json")
        .nth(1)
        .expect("the context section shows the block it documents")
        .split("```")
        .next()
        .expect("split always yields a first piece");
    serde_json::from_str(fenced).expect("the documented example is valid JSON")
}

/// The docs' worked example **is** the JSON a launch writes — every field
/// name, every nesting, every value.
///
/// Compared as parsed JSON rather than as text, so the doc stays free to
/// pretty-print and reorder; what it may not do is rename a field or respell a
/// value on one side only. Renaming `repo` to `repository` in either the doc
/// or the type turns this red, which is the whole reason the block is worth
/// documenting: a skill can rely on the published names.
#[test]
fn the_documented_example_is_the_block_a_launch_writes() {
    let (ticket, map) = documented_example_node();
    let block = launched_block(&ticket, &map, Stage::InReview);
    let emitted: Value =
        serde_json::from_str(&block).unwrap_or_else(|e| panic!("the block is JSON: {block} ({e})"));
    assert_eq!(
        emitted,
        documented_example(),
        "the documented example and the emitted block have drifted; emitted:\n{block}"
    );
}

/// The block the docs teach agents to read is the block the binary writes:
/// same prefix, same schema version.
#[test]
fn the_documented_context_block_is_the_one_a_launch_writes() {
    let (ticket, map) = documented_example_node();
    let block = launched_block(&ticket, &map, Stage::Ready);
    let section = context_section();
    assert!(
        section.contains("`ctx: <json>`"),
        "the section must quote the block's own spelling:\n{section}"
    );
    assert!(
        block.starts_with(r#"{"v":1,"#),
        "the block is versioned one-line JSON: {block}"
    );
    assert!(
        section.contains("`v`"),
        "the version gate is the first thing a reader checks:\n{section}"
    );
}

/// One backticked word, as the wire spells a value: a bare string for a unit
/// variant, and the single tag key for a variant that carries data.
fn wire_word<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value).expect("the context schema is plain data") {
        Value::String(word) => word,
        Value::Object(fields) if fields.len() == 1 => {
            fields.keys().next().expect("one key").clone()
        }
        other => panic!("a wire word is a string or a one-key object, got {other}"),
    }
}

/// The whole vocabulary of one enumerated field, as serde spells it.
fn vocabulary<T: Serialize>(values: &[T]) -> BTreeSet<String> {
    values.iter().map(wire_word).collect()
}

/// One value of every arm of the aim sum — the wire only ever sees the tag.
fn every_aim() -> Vec<Aim> {
    vec![
        Aim::Map,
        Aim::Ticket {
            number: 1,
            title: String::new(),
            ticket_type: TicketType::Build,
            stage: Launchable::Ready,
            prs: vec![],
        },
    ]
}

/// One value of every arm of the PR-status sum, likewise.
fn every_pr_status() -> Vec<PrStatus> {
    vec![
        PrStatus::Draft,
        PrStatus::Open {
            checks: Checks::Absent,
            review: Review::NotRequired,
        },
        PrStatus::Merged,
        PrStatus::Closed,
    ]
}

/// Every value each enumerated field of the block can hold, as serde spells
/// it — the answer coming from the same serializer the launch itself runs, so
/// a `rename_all` change moves this side without anyone editing it.
///
/// Adding a variant to any of these types is already a compile error in
/// `src/launch.rs`, where each word is pinned to a golden literal by an
/// exhaustive `match`; this side is what then forces the docs to learn it.
fn emitted_vocabularies() -> Vec<(&'static str, BTreeSet<String>)> {
    vec![
        ("aim", vocabulary(&every_aim())),
        (
            "ticket_type",
            vocabulary(&[
                TicketType::Build,
                TicketType::Research,
                TicketType::Task,
                TicketType::Grilling,
                TicketType::Prototype,
                TicketType::Untyped,
            ]),
        ),
        (
            "stage",
            vocabulary(&[
                Launchable::Ready,
                Launchable::Building,
                Launchable::InReview,
                Launchable::NeedsAttention,
            ]),
        ),
        ("status", vocabulary(&every_pr_status())),
        (
            "checks",
            vocabulary(&[
                Checks::Absent,
                Checks::Pending,
                Checks::Passing,
                Checks::Failing,
            ]),
        ),
        (
            "review",
            vocabulary(&[
                Review::NotRequired,
                Review::Required,
                Review::Approved,
                Review::ChangesRequested,
            ]),
        ),
    ]
}

/// The vocabulary the context section publishes for one field: the backticked
/// words on the list line that names it, which the doc promises is the
/// complete list.
fn documented_vocabulary(field: &str) -> BTreeSet<String> {
    let section = context_section();
    let marker = format!("- `{field}` — ");
    let line = section
        .lines()
        .find(|line| line.trim_start().starts_with(&marker))
        .unwrap_or_else(|| panic!("the context section must publish `{field}`'s vocabulary"))
        .split_once('—')
        .expect("the marker contains an em dash")
        .1;
    line.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// Every word the docs publish for an enumerated field is a word the types
/// serialize, and every word they serialize is published.
///
/// This is the check that makes the section's promise real. Without it the
/// docs could say `"type": "BUILD"` and every other test here would still
/// pass, because they only pin the *shape* of the block — and a skill reading
/// the docs would then look for a field the binary never writes.
#[test]
fn the_documented_vocabularies_are_the_words_the_types_serialize() {
    for (field, emitted) in emitted_vocabularies() {
        assert_eq!(
            documented_vocabulary(field),
            emitted,
            "`{field}`'s documented vocabulary and its serialized one disagree"
        );
    }
}

/// The contract is *accelerator, never precondition*, and every failure mode
/// resolves to the same move — discard the block whole and discover as a
/// hand-invoked session always has.
#[test]
fn the_context_section_states_the_fallback_and_the_live_read() {
    let section = context_section();
    for phrase in [
        "accelerator, never a precondition",
        "live read",
        "ignore it entirely",
    ] {
        assert!(
            section.contains(phrase),
            "the contract must say {phrase:?}:\n{section}"
        );
    }
}

/// Every shipped skill says where it stands on the block — a consumer that
/// never learns of it would keep paying the discovery cost the block exists to
/// remove.
///
/// `wf-one` is the one skill whose answer is *never*: it is reached only by a
/// creation, which names nothing that exists yet, so its line carries no block
/// and it may not synthesize one for the subagents it hands work to. Asserting
/// that separately is the point — the substring alone was satisfied by the
/// sentence that denies the block, which is weaker than this test's name.
#[test]
fn every_bundled_skill_documents_the_context_in_its_grammar() {
    for name in wf::skills::BUNDLED {
        let doc = skill_doc(name);
        if name == "wf-one" {
            assert!(
                doc.contains("never carries a `ctx: <json>` block"),
                "{name} is a creation's skill: it must say it carries no block"
            );
        } else {
            assert!(
                doc.contains("[ctx: <json>]") || doc.contains("ctx: <json> ["),
                "{name} must show the launch context in its invocation grammar, \
                 not merely mention it in prose"
            );
        }
        assert!(
            doc.contains("GITHUB_TRACKER.md"),
            "{name} must point at the shared contract rather than restating it"
        );
    }
}
