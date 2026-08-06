//! PROTOTYPE — throwaway code for wayfinder ticket #48. Not production.
//!
//! Prints three candidate main-screen renderings against the LIVE maps of
//! blooop/wayfinder (#1, #35, #47 — deliberately hardcoded, this is a prop):
//!
//!   A. structure-first — the blocking forest is the view, status is glyphs
//!   B. grouped list (today's view) annotated with dependency signals
//!   C. leverage view — frontier/claimed roots showing what each unblocks,
//!      done collapsed to a count
//!
//! Run: cargo run --example proto_selection_views

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

const OWNER: &str = "blooop";
const NAME: &str = "wayfinder";
const MAPS: [u64; 3] = [1, 35, 47];

const QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      title
      subIssues(first: 100) {
        nodes {
          number title state
          labels(first: 10) { nodes { name } }
          assignees(first: 5) { nodes { login } }
          blockedBy(first: 50) { nodes { number state } }
          closedByPullRequestsReferences(first: 5, includeClosedPrs: true) {
            nodes { number state isDraft }
          }
        }
      }
    }
  }
}";

#[derive(Clone)]
struct T {
    number: u64,
    title: String,
    open: bool,
    assigned: bool,
    /// Every blockedBy edge, open or closed (the full DAG).
    blockers: Vec<u64>,
    /// Rendered PR badges, e.g. "PR#46 merged".
    prs: Vec<String>,
    /// wayfinder:* type short name, if any.
    typ: Option<String>,
}

struct Cluster {
    map_number: u64,
    title: String,
    tickets: BTreeMap<u64, T>,
}

// ---- ANSI helpers (raw on purpose; ratatui comes later, this is a prop) ----
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

#[derive(PartialEq, Clone, Copy)]
enum S {
    Frontier,
    Claimed,
    Blocked,
    Done,
}

impl Cluster {
    fn status(&self, t: &T) -> S {
        if !t.open {
            S::Done
        } else if t.assigned {
            S::Claimed
        } else if t
            .blockers
            .iter()
            .any(|b| self.tickets.get(b).map(|b| b.open).unwrap_or(false))
        {
            S::Blocked
        } else {
            S::Frontier
        }
    }

    /// Direct dependents: tickets whose blockedBy lists `n`.
    fn unblocks(&self, n: u64) -> Vec<u64> {
        self.tickets
            .values()
            .filter(|t| t.blockers.contains(&n))
            .map(|t| t.number)
            .collect()
    }

    fn counts(&self) -> [usize; 4] {
        let mut c = [0; 4];
        for t in self.tickets.values() {
            c[match self.status(t) {
                S::Frontier => 0,
                S::Claimed => 1,
                S::Blocked => 2,
                S::Done => 3,
            }] += 1;
        }
        c
    }
}

fn glyph(s: S) -> (char, &'static str) {
    match s {
        S::Frontier => ('○', GREEN),
        S::Claimed => ('◐', YELLOW),
        S::Blocked => ('⊘', RED),
        S::Done => ('●', DIM),
    }
}

fn row(c: &Cluster, t: &T, extra: &str) -> String {
    let s = c.status(t);
    let (g, color) = glyph(s);
    let typ = t
        .typ
        .as_deref()
        .map(|x| format!(" {DIM}[{x}]{RESET}"))
        .unwrap_or_default();
    let prs = t
        .prs
        .iter()
        .map(|p| format!(" {MAGENTA}⇄ {p}{RESET}"))
        .collect::<String>();
    let dim = if s == S::Done { DIM } else { "" };
    format!("{color}{g}{RESET} {dim}#{:<3} {}{RESET}{typ}{prs}{extra}", t.number, t.title)
}

fn header(c: &Cluster) {
    let [f, cl, b, d] = c.counts();
    println!(
        "{BOLD}{CYAN}▌ {} · {}{RESET}   {GREEN}○{f}{RESET} {YELLOW}◐{cl}{RESET} {RED}⊘{b}{RESET} {DIM}●{d}{RESET}",
        NAME, c.title
    );
}

// ---- Variant A: structure-first forest --------------------------------------
fn variant_a(c: &Cluster) {
    header(c);
    // Primary parent = lowest-numbered in-map blocker; extra blockers annotate.
    let mut children: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut roots: Vec<u64> = vec![];
    for t in c.tickets.values() {
        let in_map: Vec<u64> =
            t.blockers.iter().copied().filter(|b| c.tickets.contains_key(b)).collect();
        match in_map.iter().min() {
            Some(&p) => children.entry(p).or_default().push(t.number),
            None => roots.push(t.number),
        }
    }
    fn walk(c: &Cluster, children: &BTreeMap<u64, Vec<u64>>, n: u64, prefix: &str, last: bool, top: bool) {
        let t = &c.tickets[&n];
        let mut in_map: Vec<u64> =
            t.blockers.iter().copied().filter(|b| c.tickets.contains_key(b)).collect();
        in_map.sort_unstable();
        let also: String = if in_map.len() > 1 {
            let rest: Vec<String> = in_map.iter().skip(1).map(|b| format!("#{b}")).collect();
            format!("  {DIM}⤷ also needs {}{RESET}", rest.join(", "))
        } else {
            String::new()
        };
        let branch = if top {
            String::new()
        } else if last {
            format!("{prefix}└─")
        } else {
            format!("{prefix}├─")
        };
        println!("  {branch}{}", row(c, t, &also));
        let kids = children.get(&n).cloned().unwrap_or_default();
        let next = if top {
            prefix.to_string()
        } else if last {
            format!("{prefix}  ")
        } else {
            format!("{prefix}│ ")
        };
        for (i, k) in kids.iter().enumerate() {
            walk(c, children, *k, &next, i + 1 == kids.len(), false);
        }
    }
    for r in &roots {
        walk(c, &children, *r, "", true, true);
    }
    println!();
}

// ---- Variant B: today's grouping, annotated ---------------------------------
fn variant_b(c: &Cluster) {
    header(c);
    let labels = ["FRONTIER — ready to claim", "CLAIMED", "BLOCKED", "DONE"];
    for (gi, label) in labels.iter().enumerate() {
        let members: Vec<&T> = c
            .tickets
            .values()
            .filter(|t| {
                gi == match c.status(t) {
                    S::Frontier => 0,
                    S::Claimed => 1,
                    S::Blocked => 2,
                    S::Done => 3,
                }
            })
            .collect();
        println!("  {BOLD}{label} — {}{RESET}", members.len());
        for t in members {
            let deps = c.unblocks(t.number);
            let mut extra = String::new();
            if !deps.is_empty() {
                extra.push_str(&format!(
                    "  {GREEN}▶ unblocks {}{RESET}",
                    deps.iter().map(|d| format!("#{d}")).collect::<Vec<_>>().join(", ")
                ));
            }
            let open_needs: Vec<String> = t
                .blockers
                .iter()
                .filter(|b| c.tickets.get(b).map(|b| b.open).unwrap_or(false))
                .map(|b| format!("#{b}"))
                .collect();
            if !open_needs.is_empty() {
                extra.push_str(&format!("  {DIM}— needs {}{RESET}", open_needs.join(", ")));
            }
            println!("    {}", row(c, t, &extra));
        }
    }
    println!();
}

// ---- Variant C: leverage view — what taking each ticket unlocks -------------
fn variant_c(c: &Cluster) {
    header(c);
    fn walk_open(c: &Cluster, n: u64, prefix: &str) {
        for (i, k) in c.unblocks(n).iter().enumerate() {
            let kids = c.unblocks(n);
            let last = i + 1 == kids.len();
            let t = &c.tickets[k];
            let branch = if last { "└─" } else { "├─" };
            println!("    {prefix}{branch}{}", row(c, t, ""));
            walk_open(c, *k, &if last { format!("{prefix}  ") } else { format!("{prefix}│ ") });
        }
    }
    let mut takeable: Vec<&T> = c
        .tickets
        .values()
        .filter(|t| matches!(c.status(t), S::Frontier | S::Claimed))
        .collect();
    // Highest leverage first: most direct dependents.
    takeable.sort_by_key(|t| std::cmp::Reverse(c.unblocks(t.number).len()));
    for t in takeable {
        println!("    {}", row(c, t, ""));
        walk_open(c, t.number, "");
    }
    let done = c.counts()[3];
    let blocked_hidden = c
        .tickets
        .values()
        .filter(|t| c.status(t) == S::Blocked && {
            // blocked tickets already shown under a takeable root are not hidden
            !c.tickets.values().any(|r| {
                matches!(c.status(r), S::Frontier | S::Claimed) && t.blockers.contains(&r.number)
            })
        })
        .count();
    if blocked_hidden > 0 {
        println!("    {RED}⊘ {blocked_hidden} blocked deeper down{RESET}");
    }
    if done > 0 {
        println!("    {DIM}● {done} done (hidden){RESET}");
    }
    println!();
}

// ---- fetch -------------------------------------------------------------------
fn fetch(number: u64) -> Option<Cluster> {
    let out = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-F",
            &format!("owner={OWNER}"),
            "-F",
            &format!("name={NAME}"),
            "-F",
            &format!("number={number}"),
            "-f",
            &format!("query={QUERY}"),
        ])
        .output()
        .expect("run gh");
    if !out.status.success() {
        eprintln!("map #{number}: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let issue = &v["data"]["repository"]["issue"];
    let mut tickets = BTreeMap::new();
    for sub in issue["subIssues"]["nodes"].as_array()? {
        let number = sub["number"].as_u64()?;
        let typ = sub["labels"]["nodes"].as_array()?.iter().find_map(|l| {
            l["name"].as_str()?.strip_prefix("wayfinder:").map(str::to_string)
        });
        let prs = sub["closedByPullRequestsReferences"]["nodes"]
            .as_array()
            .map(|ns| {
                ns.iter()
                    .filter_map(|p| {
                        let s = p["state"].as_str()?.to_lowercase();
                        let d = if p["isDraft"].as_bool()? { " draft" } else { "" };
                        Some(format!("PR#{} {s}{d}", p["number"]))
                    })
                    .collect()
            })
            .unwrap_or_default();
        tickets.insert(
            number,
            T {
                number,
                title: sub["title"].as_str()?.to_string(),
                open: sub["state"] == "OPEN",
                assigned: !sub["assignees"]["nodes"].as_array()?.is_empty(),
                blockers: sub["blockedBy"]["nodes"]
                    .as_array()?
                    .iter()
                    .filter_map(|b| b["number"].as_u64())
                    .collect(),
                prs,
                typ,
            },
        );
    }
    Some(Cluster {
        map_number: number,
        title: issue["title"].as_str()?.to_string(),
        tickets,
    })
}

fn main() {
    let clusters: Vec<Cluster> = MAPS.iter().filter_map(|&n| fetch(n)).collect();
    let repos: BTreeSet<&str> = std::iter::once(NAME).collect();
    let banner = format!("wf · {} maps · {} repo(s) — all-projects view", clusters.len(), repos.len());

    for (title, f) in [
        ("VARIANT A — structure-first: the blocking forest IS the view", variant_a as fn(&Cluster)),
        ("VARIANT B — today's grouping, annotated with dependency signals", variant_b),
        ("VARIANT C — leverage view: takeable tickets and what each unlocks", variant_c),
    ] {
        println!("{BOLD}══════ {title} ══════{RESET}");
        println!("{DIM}{banner}{RESET}\n");
        for c in &clusters {
            let _ = c.map_number;
            f(c);
        }
    }
}
