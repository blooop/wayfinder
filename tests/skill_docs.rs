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

/// Every value each enumerated field of the block can hold, as serde spells
/// it — the answer coming from the same serializer the launch itself runs, so
/// a `rename_all` change moves this side without anyone editing it.
///
/// The value lists are the types' own (`every`/`every_arm`, #133), where the
/// compiler holds each list complete — not restatements of them, which a new
/// variant would silently sit out of. Adding a variant therefore cannot green
/// until the doc publishes its word, on top of the compile error in
/// `src/launch.rs` where the word itself gets pinned.
fn emitted_vocabularies() -> Vec<(&'static str, BTreeSet<String>)> {
    vec![
        ("aim", vocabulary(&Aim::every_arm())),
        ("ticket_type", vocabulary(&TicketType::every())),
        ("stage", vocabulary(&Launchable::every())),
        ("status", vocabulary(&PrStatus::every_arm())),
        ("checks", vocabulary(&Checks::every())),
        ("review", vocabulary(&Review::every())),
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

/// The contract prose itself, pinned whole rather than phrase-sampled.
///
/// The other tests bind the schema — field names, vocabularies, the worked
/// example. This binds the *rule* those fields exist to serve: orient from
/// the block, verify live before any write, and your arguments win. Mutation
/// showed that rule could be inverted wholesale ("the block beats your
/// arguments") with every schema test green (#133), because the phrase
/// checks below sample words an inverted sentence still contains. Equality
/// on the two normative sentences leaves an editor free to move them, not to
/// reverse them: changing the contract now means changing this literal in
/// the same diff, which is the two-sided edit a contract change owes.
#[test]
fn the_contract_sentences_read_exactly_as_the_contract_rules() {
    let section = context_section();
    let sentence = |lead: &str| {
        section
            .lines()
            .find(|line| line.starts_with(lead))
            .unwrap_or_else(|| {
                panic!("the context section must keep the sentence opening {lead:?}")
            })
            .to_string()
    };
    assert_eq!(
        sentence("A launch line may carry"),
        "A launch line may carry `ctx: <json>` — a snapshot of what `wf` knew at exec time. \
         It is an accelerator, never a precondition: use it to skip discovery reads (which \
         map, which PR, what type and stage); never let it substitute for a live read before \
         any write — claiming, commenting, closing, gating. If it is absent, does not parse, \
         has a `v` you don't recognise, or names a repo or ticket other than the one your \
         arguments and pinned `$REPO` name, ignore it entirely and discover via the commands \
         below, as a hand-invoked session always does."
    );
    assert_eq!(
        sentence("**Precedence:"),
        "**Precedence: your arguments beat the block, and the tracker beats both.** The \
         ticket number in your invocation is the assignment; a block naming a different \
         number or repo is discarded whole, never merged field by field."
    );
}

/// One README paragraph, unwrapped: the lines from the one opening with
/// `lead` to the next blank line, joined the way a reader reads them.
fn readme_paragraph(lead: &str) -> String {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("the README ships in this repo");
    let from = readme
        .lines()
        .skip_while(|line| !line.starts_with(lead))
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert!(
        !from.is_empty(),
        "the README must keep the paragraph opening {lead:?}"
    );
    from.join(" ")
}

/// The README promises the block only where a skill receives it.
///
/// It said "Every launch of a node", which reads as including the plain
/// mode — a mode whose whole point is that no skill runs, so there is nobody
/// to address a block to and none is emitted (the binary's own test,
/// `nothing_that_has_no_skill_to_address_is_handed_context`). A reader
/// following the README would expect a block where none arrives. Pinned by
/// equality like the contract sentences above, so contradicting the
/// plain-mode statement — or quietly widening the promise again — turns this
/// red rather than surviving on sampled phrases (#133).
#[test]
fn the_readme_promises_the_block_only_where_a_skill_receives_it() {
    assert_eq!(
        readme_paragraph("**Every skill launch of a node"),
        "**Every skill launch of a node also hands the agent what `wf` already knew**, as a \
         `ctx: <json>` block between the skill's arguments and any steering suffix:"
    );
    assert_eq!(
        readme_paragraph("That is the parent map"),
        "That is the parent map, the ticket's type and stage, and its linked PRs — the \
         three serial `gh` calls a launched skill used to open with, answered before it \
         starts. It is an accelerator and never a precondition: a skill invoked by hand \
         never went through the picker, finds no block, and discovers exactly as before. \
         The one thing it deliberately cannot say is whether the ticket is still yours to \
         take — there is no assignee and no ticket status in the schema, so claiming stays \
         a live call and a stale block cannot make an agent act on someone else's work. \
         The creation rows carry no block at all: they name nothing that exists yet, and \
         the plain rows carry none either — a block is addressed to a skill, and plain \
         runs none."
    );
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
