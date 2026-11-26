// SiteOne Crawler - Run-over-run diff
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Compares two crawl runs and surfaces what changed between them:
//   - new / fixed 404 pages
//   - changed <title> text on pages that exist in both runs
//   - new / fixed orphan pages
//   - the overall quality-score delta
//
// The previous run is read from a JSON report (`--diff-previous=<path>`); the current run is
// read straight from the in-memory `Status`. Both go through the same conceptual facts, so the
// comparison is symmetric regardless of the output format used.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;

use serde_json::Value;

use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::result::status::Status;
use crate::utils;

/// APL code of the diff table and of the summary items it contributes.
pub const SUPER_TABLE_DIFF: &str = "run-over-run-diff";

/// A score movement smaller than this is treated as "flat" (no regression / no improvement).
const SCORE_EPSILON: f64 = 0.05;

/// The run-over-run facts the diff cares about.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunFacts {
    /// URLs that returned HTTP 404 (normalized).
    pub http_404: BTreeSet<String>,
    /// Page key -> `<title>` text (page key normalized).
    pub titles: BTreeMap<String, String>,
    /// Orphan-page URLs (normalized).
    pub orphans: BTreeSet<String>,
    /// Overall quality score, if the run produced one.
    pub overall_score: Option<f64>,
}

/// The result of comparing a previous run against the current one.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffReport {
    pub new_404s: Vec<String>,
    pub fixed_404s: Vec<String>,
    /// (page key, previous title, current title) for pages present in both runs whose title changed.
    pub changed_titles: Vec<(String, String, String)>,
    pub new_orphans: Vec<String>,
    pub fixed_orphans: Vec<String>,
    pub score_prev: Option<f64>,
    pub score_now: Option<f64>,
}

/// Normalize a full URL for cross-run comparison: trim whitespace and drop a trailing "?" or "/".
/// The leading "/" is always kept (so "/" and "" both normalize to "/").
pub fn normalize_url(url: &str) -> String {
    let t = url.trim();
    if t.is_empty() {
        return "/".to_string();
    }
    let t = t.strip_suffix('?').unwrap_or(t);
    let t = if t.len() > 1 { t.trim_end_matches('/') } else { t };
    if t.is_empty() { "/".to_string() } else { t.to_string() }
}

/// Normalize a page key (a path, or a full URL) to its path+query portion, so a full URL and a
/// bare path compare equal. Always returns a value starting with "/".
pub fn normalize_page_key(key: &str) -> String {
    let t = key.trim();
    if let Some(scheme_end) = t.find("://") {
        let after_host = &t[scheme_end + 3..];
        if let Some(slash) = after_host.find('/') {
            let path = &after_host[slash..];
            return path.trim_end_matches('/').to_string();
        }
        return "/".to_string();
    }
    if t.is_empty() {
        return "/".to_string();
    }
    let t = if t.len() > 1 { t.trim_end_matches('/') } else { t };
    if t.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", t.trim_start_matches('/'))
    }
}

/// Parse the leading integer of a status string such as "404 Not Found".
fn status_code_from_str(s: &str) -> Option<i32> {
    s.split_whitespace().next()?.parse::<i32>().ok()
}

impl RunFacts {
    /// Extract facts from a parsed JSON report (used for the previous run).
    pub fn from_json(value: &Value) -> Option<RunFacts> {
        let mut http_404 = BTreeSet::new();
        if let Some(results) = value.get("results").and_then(|r| r.as_array()) {
            for row in results {
                let url = row.get("url").and_then(|u| u.as_str()).unwrap_or_default();
                let status_str = row.get("status").and_then(|s| s.as_str()).unwrap_or_default();
                if status_code_from_str(status_str) == Some(404) && !url.is_empty() {
                    http_404.insert(normalize_url(url));
                }
            }
        }

        let mut titles = BTreeMap::new();
        if let Some(rows) = value
            .get("tables")
            .and_then(|t| t.get("seo"))
            .and_then(|t| t.get("rows"))
            .and_then(|r| r.as_array())
        {
            for row in rows {
                let key = row.get("urlPathAndQuery").and_then(|k| k.as_str()).unwrap_or_default();
                let title = row.get("title").and_then(|t| t.as_str()).unwrap_or_default();
                if !key.is_empty() {
                    titles.insert(normalize_page_key(key), title.to_string());
                }
            }
        }

        let mut orphans = BTreeSet::new();
        if let Some(rows) = value
            .get("tables")
            .and_then(|t| t.get("orphan-pages"))
            .and_then(|t| t.get("rows"))
            .and_then(|r| r.as_array())
        {
            for row in rows {
                let url = row.get("page").and_then(|u| u.as_str()).unwrap_or_default();
                if !url.is_empty() {
                    orphans.insert(normalize_url(url));
                }
            }
        }

        let overall_score = value
            .get("qualityScores")
            .and_then(|q| q.get("overall"))
            .and_then(|o| o.get("score"))
            .and_then(|s| s.as_f64());

        Some(RunFacts {
            http_404,
            titles,
            orphans,
            overall_score,
        })
    }

    /// Extract facts from a previous run's JSON report file on disk.
    pub fn from_json_path(path: &str) -> Option<RunFacts> {
        let text = fs::read_to_string(path).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        RunFacts::from_json(&value)
    }

    /// Extract facts from the current in-memory crawl result.
    pub fn from_status(status: &Status, overall_score: Option<f64>) -> RunFacts {
        let mut http_404 = BTreeSet::new();
        for v in status.get_visited_urls() {
            if v.status_code == 404 {
                http_404.insert(normalize_url(&v.url));
            }
        }

        let mut titles = BTreeMap::new();
        for row in status.get_super_table_rows("seo") {
            let key = row.get("urlPathAndQuery").cloned().unwrap_or_default();
            let title = row.get("title").cloned().unwrap_or_default();
            if !key.is_empty() {
                titles.insert(normalize_page_key(&key), title);
            }
        }

        let mut orphans = BTreeSet::new();
        for row in status.get_super_table_rows("orphan-pages") {
            let url = row.get("page").cloned().unwrap_or_default();
            if !url.is_empty() {
                orphans.insert(normalize_url(&url));
            }
        }

        RunFacts {
            http_404,
            titles,
            orphans,
            overall_score,
        }
    }
}

impl DiffReport {
    /// The overall-score movement (current - previous), if both runs have a score.
    pub fn score_delta(&self) -> Option<f64> {
        match (self.score_now, self.score_prev) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        }
    }

    /// A regression is: any new 404 page, any new orphan page, or a score drop beyond the
    /// epsilon. Title changes are neutral (they can be either improvement or regression).
    pub fn has_regression(&self) -> bool {
        if !self.new_404s.is_empty() || !self.new_orphans.is_empty() {
            return true;
        }
        if let Some(delta) = self.score_delta()
            && delta < -SCORE_EPSILON
        {
            return true;
        }
        false
    }

    /// Total number of individual change rows (for the summary headline).
    pub fn total_changes(&self) -> usize {
        let mut n = self.new_404s.len()
            + self.fixed_404s.len()
            + self.changed_titles.len()
            + self.new_orphans.len()
            + self.fixed_orphans.len();
        if self.score_delta().is_some() {
            n += 1;
        }
        n
    }

    /// A single-line human-readable summary of the diff (for the summary section).
    pub fn summary_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.new_404s.is_empty() {
            parts.push(format!("{} new 404", self.new_404s.len()));
        }
        if !self.fixed_404s.is_empty() {
            parts.push(format!("{} fixed 404", self.fixed_404s.len()));
        }
        if !self.changed_titles.is_empty() {
            parts.push(format!("{} changed title", self.changed_titles.len()));
        }
        if !self.new_orphans.is_empty() {
            parts.push(format!("{} new orphan", self.new_orphans.len()));
        }
        if !self.fixed_orphans.is_empty() {
            parts.push(format!("{} fixed orphan", self.fixed_orphans.len()));
        }
        if let Some(delta) = self.score_delta() {
            parts.push(format!("score {delta:+.2}"));
        }
        if parts.is_empty() {
            "no differences".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// Render the report as a two-column `Change` / `Detail` table.
    pub fn to_super_table(&self) -> SuperTable {
        let console_width = utils::get_console_width();
        let change_width: i32 = 16;
        let detail_width = (console_width as i32 - change_width - 4).clamp(40, 160);

        let change_col = SuperTableColumn::new(
            "change".to_string(),
            "Change".to_string(),
            change_width,
            None,
            None,
            false,
            false,
            false,
            true,
            None,
        );
        let detail_col = SuperTableColumn::new(
            "detail".to_string(),
            "Detail".to_string(),
            detail_width,
            None,
            None,
            true,
            false,
            false,
            true,
            None,
        );

        let mut data: Vec<HashMap<String, String>> = Vec::new();
        let push = |data: &mut Vec<HashMap<String, String>>, change: &str, detail: String| {
            let mut row = HashMap::new();
            row.insert("change".to_string(), change.to_string());
            row.insert("detail".to_string(), detail);
            data.push(row);
        };

        for u in &self.new_404s {
            push(&mut data, "New 404 page", u.clone());
        }
        for u in &self.fixed_404s {
            push(&mut data, "Fixed 404 page", u.clone());
        }
        for (page, prev_t, cur_t) in &self.changed_titles {
            push(
                &mut data,
                "Title changed",
                format!(
                    "{} \u{2014} \u{201c}{}\u{201d} \u{2192} \u{201c}{}\u{201d}",
                    page, prev_t, cur_t
                ),
            );
        }
        for u in &self.new_orphans {
            push(&mut data, "New orphan page", u.clone());
        }
        for u in &self.fixed_orphans {
            push(&mut data, "Fixed orphan page", u.clone());
        }
        if let Some(delta) = self.score_delta() {
            let prev = self.score_prev.map(|v| format!("{v:.2}")).unwrap_or_else(|| "?".into());
            let now = self.score_now.map(|v| format!("{v:.2}")).unwrap_or_else(|| "?".into());
            push(&mut data, "Score", format!("{prev} \u{2192} {now} ({delta:+.2})"));
        }

        let mut table = SuperTable::new(
            SUPER_TABLE_DIFF.to_string(),
            "Run-over-run diff".to_string(),
            "No differences from the previous run.".to_string(),
            vec![change_col, detail_col],
            true,
            Some("change".to_string()),
            "ASC".to_string(),
            Some(
                "Differences between this crawl and the previous run: new/fixed 404 pages, \
                 changed titles, new/fixed orphan pages, and the overall-score delta."
                    .to_string(),
            ),
            None,
            None,
        );
        table.set_data(data);
        table
    }
}

/// Compare a previous run's facts against the current run's facts.
pub fn compare(prev: &RunFacts, cur: &RunFacts) -> DiffReport {
    let new_404s: Vec<String> = cur.http_404.difference(&prev.http_404).cloned().collect();
    let fixed_404s: Vec<String> = prev.http_404.difference(&cur.http_404).cloned().collect();

    let new_orphans: Vec<String> = cur.orphans.difference(&prev.orphans).cloned().collect();
    let fixed_orphans: Vec<String> = prev.orphans.difference(&cur.orphans).cloned().collect();

    let mut changed_titles = Vec::new();
    let mut all_pages: BTreeSet<String> = BTreeSet::new();
    all_pages.extend(cur.titles.keys().cloned());
    all_pages.extend(prev.titles.keys().cloned());
    for page in all_pages {
        if let (Some(c), Some(p)) = (cur.titles.get(&page), prev.titles.get(&page))
            && c.trim() != p.trim()
        {
            changed_titles.push((page.clone(), p.clone(), c.clone()));
        }
    }
    changed_titles.sort();

    DiffReport {
        new_404s,
        fixed_404s,
        changed_titles,
        new_orphans,
        fixed_orphans,
        score_prev: prev.overall_score,
        score_now: cur.overall_score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_facts(urls_404: &[&str], titles: &[(&str, &str)], orphans: &[&str], score: Option<f64>) -> Value {
        let mut results = Vec::new();
        for u in urls_404 {
            results.push(serde_json::json!({ "url": u, "status": "404 Not Found" }));
        }
        // also add a non-404 row to make sure it's filtered out
        results.push(serde_json::json!({ "url": "https://example.com/ok", "status": "200 OK" }));

        let seo_rows: Vec<Value> = titles
            .iter()
            .map(|(k, t)| serde_json::json!({ "urlPathAndQuery": k, "title": t }))
            .collect();
        let orphan_rows: Vec<Value> = orphans.iter().map(|u| serde_json::json!({ "page": u })).collect();

        let score_value = score.map(|s| serde_json::json!(s));
        serde_json::json!({
            "results": results,
            "tables": {
                "seo": { "rows": seo_rows },
                "orphan-pages": { "rows": orphan_rows },
            },
            "qualityScores": { "overall": { "score": score_value } },
        })
    }

    #[test]
    fn from_json_extracts_facts() {
        let v = json_facts(
            &["https://example.com/old", "https://example.com/gone"],
            &[("/a", "A old"), ("/b", "B")],
            &["https://example.com/orphan1"],
            Some(8.5),
        );
        let facts = RunFacts::from_json(&v).unwrap();
        assert_eq!(
            facts.http_404,
            [
                "https://example.com/old".to_string(),
                "https://example.com/gone".to_string(),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );
        assert!(!facts.http_404.contains("https://example.com/ok"));
        assert_eq!(facts.titles.get("/a"), Some(&"A old".to_string()));
        assert_eq!(facts.titles.get("/b"), Some(&"B".to_string()));
        assert!(facts.orphans.contains("https://example.com/orphan1"));
        assert_eq!(facts.overall_score, Some(8.5));
    }

    #[test]
    fn compare_detects_regressions_and_fixes() {
        let prev = RunFacts {
            http_404: ["https://example.com/old".to_string()].into_iter().collect(),
            titles: [("/a".to_string(), "A old".to_string())].into_iter().collect(),
            orphans: ["https://example.com/orphan1".to_string()].into_iter().collect(),
            overall_score: Some(9.0),
        };
        let cur = RunFacts {
            http_404: [
                "https://example.com/old".to_string(),
                "https://example.com/new-broken".to_string(),
            ]
            .into_iter()
            .collect(),
            titles: [("/a".to_string(), "A new".to_string())].into_iter().collect(),
            orphans: [
                "https://example.com/orphan1".to_string(),
                "https://example.com/orphan2".to_string(),
            ]
            .into_iter()
            .collect(),
            overall_score: Some(7.0),
        };

        let report = compare(&prev, &cur);
        assert_eq!(report.new_404s, vec!["https://example.com/new-broken"]);
        assert!(report.fixed_404s.is_empty());
        assert_eq!(report.new_orphans, vec!["https://example.com/orphan2"]);
        assert!(report.fixed_orphans.is_empty());
        assert_eq!(
            report.changed_titles,
            vec![("/a".to_string(), "A old".to_string(), "A new".to_string())]
        );
        assert_eq!(report.score_delta(), Some(-2.0));
        assert!(report.has_regression());
        assert_eq!(report.total_changes(), 4); // new 404, title, new orphan, score
    }

    #[test]
    fn compare_no_regression_when_improved() {
        let prev = RunFacts {
            http_404: ["https://example.com/old".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let cur = RunFacts {
            overall_score: Some(9.0),
            ..Default::default()
        };
        let report = compare(&prev, &cur);
        // the old 404 is fixed, score improved, no new 404/orphan
        assert_eq!(report.fixed_404s, vec!["https://example.com/old"]);
        assert!(report.new_404s.is_empty());
        assert!(!report.has_regression());
    }

    #[test]
    fn score_within_epsilon_is_not_regression() {
        let prev = RunFacts {
            overall_score: Some(8.0),
            ..Default::default()
        };
        let cur = RunFacts {
            overall_score: Some(7.96),
            ..Default::default()
        };
        let report = compare(&prev, &cur);
        assert!(report.score_delta().is_some());
        assert!(!report.has_regression());
    }

    #[test]
    fn unchanged_titles_are_not_reported() {
        let prev = RunFacts {
            titles: [("/a".to_string(), "Same".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let cur = RunFacts {
            titles: [("/a".to_string(), "Same  ".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let report = compare(&prev, &cur);
        assert!(report.changed_titles.is_empty());
    }

    #[test]
    fn new_and_removed_pages_are_not_title_changes() {
        let prev = RunFacts {
            titles: [("/a".to_string(), "A".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let cur = RunFacts {
            titles: [("/b".to_string(), "B".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let report = compare(&prev, &cur);
        assert!(report.changed_titles.is_empty());
    }

    #[test]
    fn normalize_url_trims_trailing_slash_and_question() {
        assert_eq!(normalize_url("https://example.com/a/"), "https://example.com/a");
        assert_eq!(normalize_url("https://example.com/a?"), "https://example.com/a");
        assert_eq!(normalize_url("https://example.com/"), "https://example.com");
        assert_eq!(normalize_url("https://example.com/   "), "https://example.com");
        assert_eq!(normalize_url("/"), "/");
        assert_eq!(normalize_url(""), "/");
    }

    #[test]
    fn normalize_page_key_unifies_path_and_full_url() {
        assert_eq!(normalize_page_key("/blog/post"), "/blog/post");
        assert_eq!(normalize_page_key("https://example.com/blog/post"), "/blog/post");
        assert_eq!(normalize_page_key("/"), "/");
        assert_eq!(normalize_page_key("https://example.com"), "/");
        assert_eq!(normalize_page_key("/a/"), "/a");
    }

    #[test]
    fn to_super_table_renders_all_change_rows() {
        let report = DiffReport {
            new_404s: vec!["https://example.com/n1".to_string()],
            fixed_404s: vec!["https://example.com/f1".to_string()],
            changed_titles: vec![("/a".to_string(), "old".to_string(), "new".to_string())],
            new_orphans: vec!["https://example.com/o1".to_string()],
            fixed_orphans: vec![],
            score_prev: Some(8.0),
            score_now: Some(7.5),
        };
        let table = report.to_super_table();
        assert_eq!(table.apl_code, SUPER_TABLE_DIFF);
        let rows = table.get_data();
        assert_eq!(rows.len(), 5);
        // every row has both columns populated
        for r in rows {
            assert!(r.contains_key("change"));
            assert!(r.contains_key("detail"));
        }
    }

    #[test]
    fn to_super_table_empty_report() {
        let report = DiffReport {
            new_404s: vec![],
            fixed_404s: vec![],
            changed_titles: vec![],
            new_orphans: vec![],
            fixed_orphans: vec![],
            score_prev: None,
            score_now: None,
        };
        assert!(report.to_super_table().get_data().is_empty());
        assert_eq!(report.total_changes(), 0);
    }
}
