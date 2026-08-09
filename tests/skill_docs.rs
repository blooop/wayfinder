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
/// Built through the binary's own launch path rather than quoted, because the
/// point of these checks is that the docs and the code cannot drift: a bumped
/// schema version or a re-spelt block prefix has to fail here.
fn launched_prompt() -> String {
    let checkout = wf::projects::Checkout {
        path: std::path::PathBuf::from("/data/proj/wayfinder"),
        repo: "blooop/wayfinder".to_string(),
    };
    let ticket = wf::model::Ticket {
        repo: "blooop/wayfinder".to_string(),
        number: 16,
        title: "the ticket".to_string(),
        status: wf::model::classify(true, false, vec![]),
        ticket_type: wf::model::TicketType::Build,
        blocked_by: vec![],
        prs: vec![],
    };
    let map = wf::launch::MapRef::new(&wf::model::MapId::new("blooop/wayfinder", 1), "the map");
    let staged = wf::launch::Staged::ticket(&ticket, &map, wf::model::Stage::Ready)
        .expect("ready is launchable");
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

/// The block the docs teach agents to read is the block the binary writes:
/// same prefix, same schema version.
#[test]
fn the_documented_context_block_is_the_one_a_launch_writes() {
    let prompt = launched_prompt();
    let section = context_section();
    let block = prompt
        .split_once(" ctx: ")
        .unwrap_or_else(|| panic!("a launched skill carries its context: {prompt}"))
        .1;
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

/// Every shipped skill names the block in its own invocation grammar — a
/// consumer that never learns of it would keep paying the discovery cost the
/// block exists to remove.
#[test]
fn every_bundled_skill_documents_the_context_in_its_grammar() {
    for name in wf::skills::BUNDLED {
        let doc = skill_doc(name);
        assert!(
            doc.contains("ctx: <json>"),
            "{name} must name the launch context in its invocation grammar"
        );
        assert!(
            doc.contains("GITHUB_TRACKER.md"),
            "{name} must point at the shared contract rather than restating it"
        );
    }
}
