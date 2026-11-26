// SiteOne Crawler - MobileFriendlinessAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Checks whether pages are mobile-friendly using the signals Google's mobile-friendliness test
// relies on: a <meta name="viewport"> with width=device-width, pinch-zoom allowed, and a legible
// base font size. Pages without a proper viewport render as tiny, non-reflowing content on small
// screens, which directly hurts mobile rankings, Core Web Vitals and basic usability.
//
// Deterministic; no network, no LLM. Tappable hit-target spacing needs live rendered geometry and
// is intentionally out of scope for this static analyzer.

use std::collections::HashMap;
use std::time::Instant;

use once_cell::sync::Lazy;
use regex::Regex;
use scraper::Selector;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::analysis::result::analyzer_stats::AnalyzerStats;
use crate::analysis::result::url_analysis_result::UrlAnalysisResult;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::extra_column::ExtraColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::VisitedUrl;
use crate::types::ContentTypeId;
use crate::utils;

const ANALYSIS_MOBILE: &str = "Mobile friendliness";
/// A base font-size at or below this (px) is treated as too small to read comfortably on a phone.
const MIN_BASE_FONT_PX: u32 = 12;

/// A base `font-size` declaration in px scoped to html/body/:root in the page's own CSS.
static RE_BASE_FONT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:html|body|:root)[^{]*\{[^}]*?font-size\s*:\s*(\d+)px").unwrap());

pub struct MobileFriendlinessAnalyzer {
    base: BaseAnalyzer,
    stats: AnalyzerStats,
    pages_checked: usize,
    pages_with_issues: usize,
    pages_without_viewport: usize,
    pages_fixed_viewport: usize,
    pages_zoom_blocked: usize,
    pages_small_base_font: usize,
}

impl MobileFriendlinessAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
            stats: AnalyzerStats::new(),
            pages_checked: 0,
            pages_with_issues: 0,
            pages_without_viewport: 0,
            pages_fixed_viewport: 0,
            pages_zoom_blocked: 0,
            pages_small_base_font: 0,
        }
    }

    /// The first matching element's `attr` value (lowercased), or None.
    fn first_attr(doc: &scraper::Html, selector: &str, attr: &str) -> Option<String> {
        if let Ok(sel) = Selector::parse(selector) {
            for el in doc.select(&sel) {
                if let Some(v) = el.value().attr(attr) {
                    return Some(v.to_lowercase());
                }
            }
        }
        None
    }

    /// Analyse one page. Returns (severity, findings) where severity is "ok" | "notice" | "warning".
    fn analyze_page(html: &str) -> (&'static str, Vec<String>) {
        let doc = scraper::Html::parse_document(html);
        let mut findings: Vec<String> = Vec::new();

        let viewport = Self::first_attr(&doc, "meta[name=viewport]", "content");
        let vp_lower = match viewport {
            Some(v) => v,
            None => {
                findings.push("No <meta name=\"viewport\"> tag (page is not mobile-optimized)".to_string());
                return ("warning", findings);
            }
        };

        if !vp_lower.contains("width=device-width") {
            findings.push(format!(
                "Viewport width is not set to device-width (content: '{}')",
                vp_lower
            ));
        }

        let zoom_disabled = vp_lower.contains("user-scalable=no")
            || vp_lower.contains("user-scalable=false")
            || vp_lower.contains("maximum-scale=1");
        if zoom_disabled {
            findings.push("Viewport disables pinch-zoom (accessibility issue)".to_string());
        }

        // Base font legibility: best effort over the page's own <style> and inline styles.
        let mut style_text = String::new();
        if let Ok(sel) = Selector::parse("style") {
            for el in doc.select(&sel) {
                style_text.push_str(&el.text().collect::<Vec<&str>>().join(" "));
            }
        }
        if let Ok(sel) = Selector::parse("[style]") {
            for el in doc.select(&sel) {
                if let Some(v) = el.value().attr("style") {
                    style_text.push(' ');
                    style_text.push_str(v);
                }
            }
        }
        if let Some(min) = RE_BASE_FONT
            .captures_iter(&style_text)
            .filter_map(|c| c.get(1))
            .filter_map(|m| m.as_str().parse::<u32>().ok())
            .min()
            && min < MIN_BASE_FONT_PX
        {
            findings.push(format!(
                "Base font size {}px is small for mobile (below {}px)",
                min, MIN_BASE_FONT_PX
            ));
        }

        if findings.is_empty() {
            return ("ok", findings);
        }
        if !vp_lower.contains("width=device-width") || zoom_disabled {
            return ("warning", findings);
        }
        ("notice", findings)
    }

    fn set_findings_to_summary(&self, status: &Status) {
        if self.pages_checked == 0 {
            status.add_notice_to_summary(
                "mobile-friendliness",
                "No HTML pages were available to assess for mobile-friendliness.",
            );
        } else if self.pages_with_issues == 0 {
            status.add_ok_to_summary(
                "mobile-friendliness",
                &format!(
                    "All {} HTML page(s) are mobile-friendly (viewport present, zoom enabled, legible base font).",
                    self.pages_checked
                ),
            );
        } else {
            let mut cats: Vec<String> = Vec::new();
            if self.pages_fixed_viewport > 0 {
                cats.push(format!(
                    "{} with a fixed (non-device-width) viewport",
                    self.pages_fixed_viewport
                ));
            }
            if self.pages_without_viewport > 0 {
                cats.push(format!("{} without a viewport tag", self.pages_without_viewport));
            }
            if self.pages_zoom_blocked > 0 {
                cats.push(format!("{} blocking pinch-zoom", self.pages_zoom_blocked));
            }
            if self.pages_small_base_font > 0 {
                cats.push(format!(
                    "{} with a base font below {}px",
                    self.pages_small_base_font, MIN_BASE_FONT_PX
                ));
            }
            let detail = if cats.is_empty() {
                "viewport width not set to device-width".to_string()
            } else {
                cats.join("; ")
            };
            if self.pages_without_viewport > 0 || self.pages_zoom_blocked > 0 {
                status.add_warning_to_summary(
                    "mobile-friendliness",
                    &format!(
                        "{} of {} HTML page(s) have a mobile-friendliness issue: {}.",
                        self.pages_with_issues, self.pages_checked, detail
                    ),
                );
            } else {
                status.add_notice_to_summary(
                    "mobile-friendliness",
                    &format!(
                        "{} of {} HTML page(s) have a minor mobile-friendliness note: {}.",
                        self.pages_with_issues, self.pages_checked, detail
                    ),
                );
            }
        }
    }
}

impl Default for MobileFriendlinessAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for MobileFriendlinessAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

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

        let data = self.stats.to_table_data();

        let mut super_table = SuperTable::new(
            "mobile-friendliness".to_string(),
            "Mobile friendliness".to_string(),
            "Nothing to report.".to_string(),
            columns,
            true,
            None,
            "ASC".to_string(),
            Some(
                "Per-page mobile-friendliness result: viewport presence, width=device-width, pinch-zoom allowance and base font size."
                    .to_string(),
            ),
            None,
            None,
        );
        super_table.set_data(data);
        status.configure_super_table_url_stripping(&mut super_table);
        output.add_super_table(&super_table);
        status.add_super_table_at_end(super_table);

        self.set_findings_to_summary(status);
        self.base.measure_exec_time("MobileFriendlinessAnalyzer", "analyze", s);
    }

    fn analyze_visited_url(
        &mut self,
        visited_url: &VisitedUrl,
        body: Option<&str>,
        _headers: Option<&HashMap<String, String>>,
    ) -> Option<UrlAnalysisResult> {
        let is_html =
            visited_url.content_type == ContentTypeId::Html && visited_url.status_code == 200 && body.is_some();
        if !is_html {
            return None;
        }
        let html = body?;
        self.pages_checked += 1;

        let (severity, findings) = Self::analyze_page(html);
        let has_issue = severity != "ok";
        if has_issue && findings.iter().any(|f| f.contains("No <meta")) {
            self.pages_without_viewport += 1;
        }
        if has_issue && findings.iter().any(|f| f.contains("Viewport width is not set")) {
            self.pages_fixed_viewport += 1;
        }
        if has_issue && findings.iter().any(|f| f.contains("pinch-zoom")) {
            self.pages_zoom_blocked += 1;
        }
        if has_issue && findings.iter().any(|f| f.contains("Base font size")) {
            self.pages_small_base_font += 1;
        }
        if has_issue {
            self.pages_with_issues += 1;
        }

        let message = findings.join("; ");
        let mut result = UrlAnalysisResult::new();
        match severity {
            "warning" => {
                result.add_warning(message, ANALYSIS_MOBILE, Some(findings));
                self.stats.add_warning(ANALYSIS_MOBILE, Some(&visited_url.url));
            }
            "notice" => {
                result.add_notice(message, ANALYSIS_MOBILE, Some(findings));
                self.stats.add_notice(ANALYSIS_MOBILE, Some(&visited_url.url));
            }
            _ => {
                result.add_ok(
                    "Mobile-friendly: viewport present, zoom enabled, legible base font".to_string(),
                    ANALYSIS_MOBILE,
                    None,
                );
                self.stats.add_ok(ANALYSIS_MOBILE, Some(&visited_url.url));
            }
        }

        Some(result)
    }

    fn show_analyzed_visited_url_result_as_column(&self) -> Option<ExtraColumn> {
        ExtraColumn::new("Mobile".to_string(), Some(8), false, None, None, None).ok()
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        128
    }

    fn get_name(&self) -> &str {
        "MobileFriendlinessAnalyzer"
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
    fn flags_missing_viewport() {
        let (sev, _) = MobileFriendlinessAnalyzer::analyze_page("<html><body><h1>Hi</h1></body></html>");
        assert_eq!(sev, "warning");
    }

    #[test]
    fn flags_zoom_disabled() {
        let (sev, findings) = MobileFriendlinessAnalyzer::analyze_page(
            "<html><head><meta name=\"viewport\" content=\"width=device-width, user-scalable=no\"></head><body>x</body></html>",
        );
        assert_eq!(sev, "warning");
        assert!(findings.iter().any(|f| f.contains("pinch-zoom")));
    }

    #[test]
    fn flags_non_device_width() {
        let (sev, _) = MobileFriendlinessAnalyzer::analyze_page(
            "<html><head><meta name=\"viewport\" content=\"width=1024\"></head><body>x</body></html>",
        );
        assert_eq!(sev, "warning");
    }

    #[test]
    fn accepts_a_good_viewport() {
        let (sev, findings) = MobileFriendlinessAnalyzer::analyze_page(
            "<html><head><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"></head><body>x</body></html>",
        );
        assert_eq!(sev, "ok");
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_tiny_base_font() {
        let (sev, findings) = MobileFriendlinessAnalyzer::analyze_page(
            "<html><head><style>body { font-size: 9px; }</style><meta name=\"viewport\" content=\"width=device-width\"></head><body>x</body></html>",
        );
        assert_eq!(sev, "notice");
        assert!(findings.iter().any(|f| f.contains("Base font size")));
    }
}
