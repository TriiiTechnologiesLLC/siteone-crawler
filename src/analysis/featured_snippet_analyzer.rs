// SiteOne Crawler - FeaturedSnippetAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Detects the on-page structure that makes an answer likely to be lifted into a featured
// snippet / AI answer box: a question-form heading (H2/H3 ending in "?") paired with a
// concise 40-80 word answer paragraph, or a structured answer (numbered/bulleted list with
// 3+ items, a table, or a definition list).
//
// Deterministic; no network, no LLM. This only reports whether the structure is present —
// it does not predict whether a specific query will actually trigger a snippet.

use std::collections::HashMap;
use std::time::Instant;

use scraper::{Html, Selector};

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::analysis::result::analyzer_stats::AnalyzerStats;
use crate::analysis::result::url_analysis_result::UrlAnalysisResult;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::VisitedUrl;
use crate::types::ContentTypeId;
use crate::utils;

const ANALYSIS_SNIPPET: &str = "Featured snippet readiness";
/// A paragraph in this word range is "concise enough" to be lifted as a snippet answer.
const CONCISE_MIN_WORDS: usize = 40;
const CONCISE_MAX_WORDS: usize = 80;
/// A list with at least this many items is a structured (numbered/bulleted) answer.
const MIN_LIST_ITEMS: usize = 3;

pub struct FeaturedSnippetAnalyzer {
    base: BaseAnalyzer,
    stats: AnalyzerStats,
    indexable_pages: usize,
    pages_snippet_ready: usize,
    pages_question_heading: usize,
    pages_concise_answer: usize,
}

impl FeaturedSnippetAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
            stats: AnalyzerStats::new(),
            indexable_pages: 0,
            pages_snippet_ready: 0,
            pages_question_heading: 0,
            pages_concise_answer: 0,
        }
    }
}

impl Default for FeaturedSnippetAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct SnippetSignals {
    question_heading: bool,
    concise_answer: bool,
    list_answer: bool,
    table: bool,
    definition: bool,
}

impl SnippetSignals {
    fn count(&self) -> usize {
        [
            self.question_heading,
            self.concise_answer,
            self.list_answer,
            self.table,
            self.definition,
        ]
        .into_iter()
        .filter(|b| *b)
        .count()
    }

    /// A page is "snippet-ready" when a question-form heading is paired with a concise or
    /// structured answer.
    fn ready(&self) -> bool {
        self.question_heading && (self.concise_answer || self.list_answer || self.table || self.definition)
    }

    fn names(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.question_heading {
            v.push("question heading".to_string());
        }
        if self.concise_answer {
            v.push("concise answer".to_string());
        }
        if self.list_answer {
            v.push("list".to_string());
        }
        if self.table {
            v.push("table".to_string());
        }
        if self.definition {
            v.push("definition list".to_string());
        }
        v
    }
}

fn text_of(el: &scraper::ElementRef) -> String {
    el.text().collect::<Vec<&str>>().join(" ").trim().to_string()
}

fn analyze_page(html: &str) -> SnippetSignals {
    let doc = Html::parse_document(html);
    let mut s = SnippetSignals::default();

    // Question-form heading (H2 or H3 ending in "?").
    if let Ok(sel) = Selector::parse("h2, h3") {
        for el in doc.select(&sel) {
            let t = text_of(&el);
            if !t.is_empty() && t.ends_with('?') {
                s.question_heading = true;
                break;
            }
        }
    }

    // Concise answer: a paragraph of 40-80 words.
    if let Ok(sel) = Selector::parse("p") {
        for el in doc.select(&sel) {
            let n = text_of(&el).split_whitespace().count();
            if (CONCISE_MIN_WORDS..=CONCISE_MAX_WORDS).contains(&n) {
                s.concise_answer = true;
                break;
            }
        }
    }

    // Structured answer: a list with >= 3 items.
    if let Ok(sel) = Selector::parse("ul li, ol li")
        && doc.select(&sel).count() >= MIN_LIST_ITEMS
    {
        s.list_answer = true;
    }

    // Table.
    if let Ok(sel) = Selector::parse("table")
        && doc.select(&sel).count() > 0
    {
        s.table = true;
    }

    // Definition list (>= 1 dt and >= 1 dd).
    if let Ok(sel) = Selector::parse("dl")
        && doc.select(&sel).count() > 0
    {
        let has_dt = Selector::parse("dt")
            .map(|d| doc.select(&d).count() > 0)
            .unwrap_or(false);
        let has_dd = Selector::parse("dd")
            .map(|d| doc.select(&d).count() > 0)
            .unwrap_or(false);
        s.definition = has_dt && has_dd;
    }

    s
}

impl Analyzer for FeaturedSnippetAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let start = Instant::now();
        let rollup = self.stats.to_table_data();

        let columns = vec![
            SuperTableColumn::new(
                "analysisName".to_string(),
                "Analysis".to_string(),
                -1,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "ok".to_string(),
                "OK".to_string(),
                5,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "green", false);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "notice".to_string(),
                "Notice".to_string(),
                6,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "blue", false);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "warning".to_string(),
                "Warning".to_string(),
                7,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "magenta", true);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "critical".to_string(),
                "Critical".to_string(),
                8,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "red", true);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut super_table = SuperTable::new(
            "featured-snippet".to_string(),
            "Featured-snippet readiness summary".to_string(),
            "Featured-snippet readiness analysis is disabled.".to_string(),
            columns,
            true,
            Some("analysisName".to_string()),
            "ASC".to_string(),
            Some(
                "On-page structure suited to featured snippets / AI answer boxes: a question-form heading paired with a concise or structured answer."
                    .to_string(),
            ),
            None,
            None,
        );
        super_table.set_data(rollup);
        status.configure_super_table_url_stripping(&mut super_table);
        output.add_super_table(&super_table);
        status.add_super_table_at_end(super_table);

        if self.indexable_pages == 0 {
            status.add_notice_to_summary(
                "featured-snippet",
                "No HTML pages were available to assess for featured-snippet readiness.",
            );
        } else if self.pages_snippet_ready == 0 {
            status.add_notice_to_summary(
                "featured-snippet",
                &format!(
                    "None of the {} HTML page(s) is structured for featured snippets (no question-form heading paired with a concise or structured answer).",
                    self.indexable_pages
                ),
            );
        } else {
            status.add_ok_to_summary(
                "featured-snippet",
                &format!(
                    "{} of {} HTML page(s) are structured for featured snippets; {} carry a question heading, {} a concise answer paragraph.",
                    self.pages_snippet_ready,
                    self.indexable_pages,
                    self.pages_question_heading,
                    self.pages_concise_answer
                ),
            );
        }

        self.base.measure_exec_time("FeaturedSnippetAnalyzer", "analyze", start);
    }

    fn analyze_visited_url(
        &mut self,
        visited_url: &VisitedUrl,
        body: Option<&str>,
        _headers: Option<&HashMap<String, String>>,
    ) -> Option<UrlAnalysisResult> {
        if !(visited_url.content_type == ContentTypeId::Html && visited_url.status_code == 200 && body.is_some()) {
            return None;
        }
        let html = body?;
        let signals = analyze_page(html);
        let mut result = UrlAnalysisResult::new();
        let names = signals.names();
        if signals.ready() {
            result.add_ok(
                format!("Structured for featured snippets: {}", names.join(", ")),
                ANALYSIS_SNIPPET,
                Some(names),
            );
            self.stats.add_ok(ANALYSIS_SNIPPET, Some(&visited_url.url));
        } else if signals.count() >= 1 {
            result.add_notice(
                format!(
                    "Partial snippet structure ({}); add a question-form heading paired with a concise 40-80 word or structured answer.",
                    names.join(", ")
                ),
                ANALYSIS_SNIPPET,
                Some(names),
            );
            self.stats.add_notice(ANALYSIS_SNIPPET, Some(&visited_url.url));
        } else {
            result.add_notice(
                "No answer-engine structure detected (no question headings, concise answers, lists, tables or definitions)."
                    .to_string(),
                ANALYSIS_SNIPPET,
                None,
            );
            self.stats.add_notice(ANALYSIS_SNIPPET, Some(&visited_url.url));
        }
        self.indexable_pages += 1;
        if signals.ready() {
            self.pages_snippet_ready += 1;
        }
        if signals.question_heading {
            self.pages_question_heading += 1;
        }
        if signals.concise_answer {
            self.pages_concise_answer += 1;
        }
        Some(result)
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        230
    }

    fn get_name(&self) -> &str {
        "FeaturedSnippetAnalyzer"
    }

    fn get_exec_times(&self) -> &HashMap<String, f64> {
        self.base.get_exec_times()
    }

    fn get_exec_counts(&self) -> &HashMap<String, usize> {
        self.base.get_exec_counts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_question_and_concise_answer() {
        let para = "This is a concise answer paragraph that explains the topic in enough detail to be lifted as a featured snippet by a search engine, covering the key points clearly and completely in a single readable block of text that a visitor can absorb quickly without scrolling.";
        let h = format!("<html><body><h2>What is SEO?</h2><p>{para}</p></body></html>");
        let s = analyze_page(&h);
        assert!(s.question_heading);
        assert!(s.concise_answer);
        assert!(s.ready());
    }

    #[test]
    fn detects_list_answer() {
        let h = "<html><body><h3>How do I start?</h3><ol><li>One</li><li>Two</li><li>Three</li></ol></body></html>";
        let s = analyze_page(h);
        assert!(s.question_heading);
        assert!(s.list_answer);
        assert!(s.ready());
    }

    #[test]
    fn no_signals_is_not_ready() {
        let h = "<html><body><h2>Company overview</h2><p>We do things.</p></body></html>";
        let s = analyze_page(h);
        assert!(!s.question_heading);
        assert!(!s.ready());
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn table_needs_a_question_to_be_ready() {
        let h = "<html><body><h2>Pricing</h2><table><tr><td>a</td></tr></table></body></html>";
        let s = analyze_page(h);
        assert!(s.table);
        assert!(!s.question_heading);
        assert!(!s.ready());
    }

    #[test]
    fn definition_list_is_a_signal() {
        let h = "<html><body><h2>What is a cookie?</h2><dl><dt>Cookie</dt><dd>A small piece of data.</dd></dl></body></html>";
        let s = analyze_page(h);
        assert!(s.definition);
        assert!(s.ready());
    }
}
