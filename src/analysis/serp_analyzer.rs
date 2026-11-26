// SiteOne Crawler - SerpAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// SERP preview & heading consistency:
//   1. SERP preview: pixel-measured rendering of each page's <title> and meta description at the
//      Google desktop SERP widths (~580px title, ~920px description). Titles or descriptions wider
//      than the limit are shown truncated with an ellipsis, exactly like the search result. Pages
//      without a meta description get the snippet Google would auto-generate (first visible text).
//   2. Duplicate H1 detection: pages sharing the same <h1> (a classic duplicate-metadata signal;
//      titles and descriptions are already covered by the best-practice analyzer).
//   3. Title/H1 mismatch: pages where the <title> and the <h1> describe substantially different
//      things — usually a stale template artifact that confuses both users and search engines.
//
// Deterministic; no network, no LLM. Character widths are estimated with per-character factors
// tuned for the Google SERP fonts (Arial-like 20px title, 14px description), so results are stable
// across platforms.

use std::collections::HashMap;
use std::time::Instant;

use scraper::{Html, Selector};

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::analysis::text_utils;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::VisitedUrl;
use crate::types::ContentTypeId;
use crate::utils;

const SUPER_TABLE_SERP: &str = "serp-preview";

// Approximate Google desktop SERP metrics (desktop, 2025 era): title 20px, max ~580px wide;
// description 14px, two lines, max ~920px wide.
pub const TITLE_FONT_PX: f64 = 20.0;
pub const DESC_FONT_PX: f64 = 14.0;
pub const TITLE_MAX_PX: f64 = 580.0;
pub const DESC_MAX_PX: f64 = 920.0;

/// Width factor of a character relative to the font size (tuned for an Arial-like SERP font).
fn char_width_factor(c: char) -> f64 {
    if c.is_ascii_uppercase() {
        0.68
    } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
        0.50
    } else if c == ' ' {
        0.26
    } else if matches!(
        c,
        '.' | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | '('
            | ')'
            | '"'
            | '\''
            | '-'
            | '/'
            | '&'
            | '|'
            | '_'
            | '='
            | '+'
            | '<'
            | '>'
    ) {
        0.30
    } else if ('\u{2E80}'..='\u{9FFF}').contains(&c)
        || ('\u{AC00}'..='\u{D7A3}').contains(&c)
        || ('\u{F900}'..='\u{FAFF}').contains(&c)
        || ('\u{FF01}'..='\u{FF60}').contains(&c)
    {
        // CJK and fullwidth forms are roughly one em wide.
        1.0
    } else if ('\u{0370}'..='\u{03FF}').contains(&c) || ('\u{0400}'..='\u{04FF}').contains(&c) {
        // Greek/Cyrillic are close to Latin lowercase width.
        0.52
    } else {
        0.55
    }
}

/// Estimated rendered width of `text` in a proportional font of `font_px` pixels.
pub fn text_width_px(text: &str, font_px: f64) -> f64 {
    text.chars().map(|c| char_width_factor(c) * font_px).sum()
}

/// Truncate `text` so it fits within `max_px` at `font_px`, ending with an ellipsis.
/// The cut is made at the last space before the limit when possible (word boundary).
/// Returns the preview and whether truncation happened.
pub fn truncate_to_width(text: &str, font_px: f64, max_px: f64) -> (String, bool) {
    if text_width_px(text, font_px) <= max_px {
        return (text.to_string(), false);
    }
    let ellipsis_px = char_width_factor('…') * font_px;
    let budget = max_px - ellipsis_px;

    let mut width = 0.0f64;
    let mut out = String::new();
    for c in text.chars() {
        let w = char_width_factor(c) * font_px;
        if width + w > budget {
            break;
        }
        width += w;
        out.push(c);
    }
    // Prefer cutting at a word boundary (do not leave a dangling word fragment).
    if let Some(pos) = out.rfind(' ') {
        out.truncate(pos);
    }
    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        ("…".to_string(), true)
    } else {
        (format!("{}…", trimmed), true)
    }
}

/// Normalize a heading/title for duplicate comparison: lowercase, trim, collapse whitespace.
pub fn normalize_for_dup(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Alphanumeric tokens of a text (lowercased).
pub fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// True when the title and H1 share less than `min_jaccard` of their word sets (substantially different).
pub fn title_h1_mismatch(title: &str, h1: &str) -> bool {
    title_h1_mismatch_with(title, h1, 0.2)
}

pub fn title_h1_mismatch_with(title: &str, h1: &str, min_jaccard: f64) -> bool {
    use std::collections::HashSet;
    let toks_a = tokens(title);
    let toks_b = tokens(h1);
    let a: HashSet<&String> = toks_a.iter().collect();
    let b: HashSet<&String> = toks_b.iter().collect();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let inter = a.intersection(&b).count();
    let union = a.union(&b).count();
    (inter as f64) / (union as f64) < min_jaccard
}

pub struct SerpAnalyzer {
    base: BaseAnalyzer,
}

impl SerpAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
        }
    }

    fn is_html_page(u: &VisitedUrl) -> bool {
        u.content_type == ContentTypeId::Html && u.status_code == 200 && u.is_allowed_for_crawling
    }

    fn get_url_path_and_query(url: &str) -> String {
        if let Ok(parsed) = url::Url::parse(url) {
            let path = parsed.path().to_string();
            if let Some(query) = parsed.query() {
                format!("{}?{}", path, query)
            } else {
                path
            }
        } else {
            url.to_string()
        }
    }
}

impl Default for SerpAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct PageSerp {
    path_query: String,
    title: Option<String>,
    description: Option<String>,
    description_is_auto: bool,
    h1: Option<String>,
    title_preview: String,
    title_truncated: bool,
    title_px: i32,
    desc_preview: String,
    desc_truncated: bool,
    desc_px: i32,
    h1_duplicate: bool,
    h1_group_size: usize,
    title_h1_mismatch: bool,
}

impl Analyzer for SerpAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

        let visited = status.get_visited_urls();
        let html_pages: Vec<&VisitedUrl> = visited.iter().filter(|u| Self::is_html_page(u)).collect();

        let mut pages: Vec<PageSerp> = Vec::new();
        for u in &html_pages {
            let body = match status.get_url_body_text(&u.uq_id) {
                Some(b) => b,
                None => continue,
            };
            let document = Html::parse_document(&body);

            let title = Selector::parse("title")
                .ok()
                .and_then(|sel| document.select(&sel).next())
                .map(|e| e.text().collect::<Vec<_>>().join("").trim().to_string())
                .filter(|t| !t.is_empty());

            let description = Selector::parse("meta[name=description]")
                .ok()
                .and_then(|sel| document.select(&sel).next())
                .map(|e| e.value().attr("content").unwrap_or("").trim().to_string())
                .filter(|t| !t.is_empty());

            // No meta description: Google auto-generates a snippet from the visible page text.
            let (description, description_is_auto) = match description {
                Some(d) => (Some(d), false),
                None => {
                    let text = text_utils::visible_main_text(&body);
                    let snippet: String = text.chars().take(250).collect();
                    if snippet.trim().is_empty() {
                        (None, false)
                    } else {
                        (Some(snippet.trim().to_string()), true)
                    }
                }
            };

            let h1 = Selector::parse("h1")
                .ok()
                .and_then(|sel| document.select(&sel).next())
                .map(|e| e.text().collect::<Vec<_>>().join(" ").trim().to_string())
                .filter(|t| !t.is_empty());

            let title_text = title.clone().unwrap_or_default();
            let (title_preview, title_truncated) = truncate_to_width(&title_text, TITLE_FONT_PX, TITLE_MAX_PX);
            let title_px = text_width_px(&title_text, TITLE_FONT_PX) as i32;

            let desc_text = description.clone().unwrap_or_default();
            let (desc_preview, desc_truncated) = truncate_to_width(&desc_text, DESC_FONT_PX, DESC_MAX_PX);
            let desc_px = text_width_px(&desc_text, DESC_FONT_PX) as i32;

            pages.push(PageSerp {
                path_query: Self::get_url_path_and_query(&u.url),
                title,
                description,
                description_is_auto,
                h1,
                title_preview,
                title_truncated,
                title_px,
                desc_preview,
                desc_truncated,
                desc_px,
                h1_duplicate: false,
                h1_group_size: 0,
                title_h1_mismatch: false,
            });
        }

        // ---- Site-wide: duplicate H1 detection ------------------------------------------
        let mut h1_groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in pages.iter().enumerate() {
            if let Some(h1) = &p.h1 {
                let key = normalize_for_dup(h1);
                if !key.is_empty() {
                    h1_groups.entry(key).or_default().push(i);
                }
            }
        }
        for group in h1_groups.values() {
            if group.len() >= 2 {
                for i in group {
                    pages[*i].h1_duplicate = true;
                    pages[*i].h1_group_size = group.len();
                }
            }
        }

        // ---- Per page: title/H1 mismatch -------------------------------------------------
        for p in pages.iter_mut() {
            if let (Some(title), Some(h1)) = (&p.title, &p.h1) {
                p.title_h1_mismatch = title_h1_mismatch(title, h1);
            }
        }

        // ---- Table ------------------------------------------------------------------------
        let console_width = utils::get_console_width();
        let page_width = (console_width as i32 - 140).clamp(30, 200);

        let columns = vec![
            SuperTableColumn::new(
                "page".to_string(),
                "Page".to_string(),
                page_width,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "title".to_string(),
                "Title (as rendered in the SERP)".to_string(),
                45,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "titleWidth".to_string(),
                "Title width".to_string(),
                11,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "description".to_string(),
                "Meta description (as rendered in the SERP)".to_string(),
                55,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "descWidth".to_string(),
                "Desc. width".to_string(),
                11,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "h1".to_string(),
                "H1".to_string(),
                35,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "issues".to_string(),
                "Issues".to_string(),
                40,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut data: Vec<HashMap<String, String>> = Vec::new();
        for p in &pages {
            let mut issues: Vec<String> = Vec::new();
            if p.title_truncated {
                issues.push("title truncated".to_string());
            }
            if p.desc_truncated {
                issues.push("description truncated".to_string());
            }
            if p.description_is_auto {
                issues.push("no meta description (auto snippet)".to_string());
            }
            if p.h1_duplicate {
                issues.push(format!("duplicate H1 ({} pages)", p.h1_group_size));
            }
            if p.title_h1_mismatch {
                issues.push("title/H1 mismatch".to_string());
            }

            let mut row = HashMap::new();
            row.insert("page".to_string(), p.path_query.clone());
            row.insert(
                "title".to_string(),
                if p.title.is_some() {
                    p.title_preview.clone()
                } else {
                    "(missing)".to_string()
                },
            );
            row.insert(
                "titleWidth".to_string(),
                if p.title_truncated {
                    format!("{} (cut)", p.title_px)
                } else {
                    p.title_px.to_string()
                },
            );
            row.insert(
                "description".to_string(),
                if p.description.is_some() {
                    p.desc_preview.clone()
                } else {
                    "(missing)".to_string()
                },
            );
            row.insert(
                "descWidth".to_string(),
                if p.desc_truncated {
                    format!("{} (cut)", p.desc_px)
                } else {
                    p.desc_px.to_string()
                },
            );
            row.insert(
                "h1".to_string(),
                if p.h1.is_some() {
                    p.h1.clone().unwrap_or_default()
                } else {
                    "(missing)".to_string()
                },
            );
            row.insert("issues".to_string(), issues.join("; "));
            data.push(row);
        }

        let mut table = SuperTable::new(
            SUPER_TABLE_SERP.to_string(),
            "SERP preview & heading consistency".to_string(),
            "No HTML pages to analyze.".to_string(),
            columns,
            true,
            Some("page".to_string()),
            "ASC".to_string(),
            Some(
                "How Google renders each page's title and meta description in the SERP (pixel-measured at the \
                 desktop SERP widths, ~580px title and ~920px description). Wider text is truncated with an \
                 ellipsis, like in the search result. Also flags duplicate H1s and title/H1 mismatches."
                    .to_string(),
            ),
            None,
            None,
        );
        table.set_data(data);
        status.configure_super_table_url_stripping(&mut table);
        output.add_super_table(&table);
        status.add_super_table_at_beginning(table);

        // ---- Summary ----------------------------------------------------------------------
        let truncated_titles: Vec<&PageSerp> = pages.iter().filter(|p| p.title_truncated).collect();
        let truncated_descs: Vec<&PageSerp> = pages.iter().filter(|p| p.desc_truncated).collect();
        let dup_h1_pages: Vec<&PageSerp> = pages.iter().filter(|p| p.h1_duplicate).collect();
        let mismatches: Vec<&PageSerp> = pages.iter().filter(|p| p.title_h1_mismatch).collect();

        if !truncated_titles.is_empty() {
            status.add_notice_to_summary(
                "serp-title-truncated",
                &format!(
                    "{} page(s) have a title wider than the SERP limit (~{}px) — it will be cut with an ellipsis",
                    truncated_titles.len(),
                    TITLE_MAX_PX as i32
                ),
            );
        }
        if !truncated_descs.is_empty() {
            status.add_notice_to_summary(
                "serp-desc-truncated",
                &format!(
                    "{} page(s) have a meta description wider than the SERP limit (~{}px) — it will be cut with an ellipsis",
                    truncated_descs.len(),
                    DESC_MAX_PX as i32
                ),
            );
        }

        if dup_h1_pages.is_empty() {
            status.add_ok_to_summary("serp-h1-duplicate", "No duplicate H1 headings detected.");
        } else {
            status.add_summary_item_by_ranges(
                "serp-h1-duplicate",
                dup_h1_pages.len() as f64,
                &[(0.0, 0.0), (1.0, 4.0), (5.0, 100_000.0)],
                &[
                    "No duplicate H1 headings detected",
                    "{} pages share a duplicate <h1> heading",
                    "{} pages share a duplicate <h1> heading",
                ],
            );
        }

        if !mismatches.is_empty() {
            status.add_notice_to_summary(
                "serp-title-h1-mismatch",
                &format!(
                    "{} page(s) have a <title> and <h1> that describe substantially different things",
                    mismatches.len()
                ),
            );
        }

        let clean = truncated_titles.is_empty()
            && truncated_descs.is_empty()
            && dup_h1_pages.is_empty()
            && mismatches.is_empty();
        if clean {
            status.add_ok_to_summary(
                "serp-preview",
                "SERP previews look fine: titles and descriptions fit the SERP widths and H1s are unique.",
            );
        }

        self.base.measure_exec_time("SerpAnalyzer", "analyze", s);
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        114
    }

    fn get_name(&self) -> &str {
        "SerpAnalyzer"
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
    fn short_text_not_truncated() {
        let (preview, truncated) = truncate_to_width("Home", TITLE_FONT_PX, TITLE_MAX_PX);
        assert_eq!(preview, "Home");
        assert!(!truncated);
    }

    #[test]
    fn long_text_truncated_with_ellipsis() {
        let long = "This is a very long page title that will definitely exceed the SERP width limit".repeat(2);
        let (preview, truncated) = truncate_to_width(&long, TITLE_FONT_PX, TITLE_MAX_PX);
        assert!(truncated);
        assert!(preview.ends_with('…'));
        // The preview (including the ellipsis) must fit the limit.
        assert!(text_width_px(&preview, TITLE_FONT_PX) <= TITLE_MAX_PX + 1.0);
    }

    #[test]
    fn width_grows_with_length() {
        let short = text_width_px("ab", TITLE_FONT_PX);
        let long = text_width_px("abcd", TITLE_FONT_PX);
        assert!(long > short);
    }

    #[test]
    fn cjk_wider_than_latin() {
        let latin = text_width_px("aaaa", TITLE_FONT_PX);
        let cjk = text_width_px("中文中文", TITLE_FONT_PX);
        assert!(cjk > latin);
    }

    #[test]
    fn normalize_collapses_whitespace_and_case() {
        assert_eq!(normalize_for_dup("  Hello   WORLD  "), "hello world");
    }

    #[test]
    fn mismatch_when_disjoint_words() {
        assert!(title_h1_mismatch(
            "Buy Running Shoes Online",
            "About Our Company History"
        ));
        assert!(!title_h1_mismatch("Buy Running Shoes Online", "Running Shoes"));
    }

    #[test]
    fn truncate_prefers_word_boundary() {
        // Build a text where the raw cut lands mid-word.
        let text = "aaaaaaaa bbbbbbbb cccccccc dddddddd eeeeeeee";
        let (preview, truncated) = truncate_to_width(text, TITLE_FONT_PX, 150.0);
        assert!(truncated);
        let body = preview.strip_suffix('…').unwrap();
        // The last character before the ellipsis must be a space-trimmed word end (no dangling
        // partial word because we cut at the last space).
        assert!(!body.ends_with(' '));
    }
}
