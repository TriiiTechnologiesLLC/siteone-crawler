// SiteOne Crawler - MixedContentAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Detects mixed content: references to insecure http:// resources embedded in an https:// page.
// Active mixed content (scripts, iframes, forms, embeds loading over plain HTTP) is blocked by
// modern browsers and breaks functionality; passive mixed content (images, CSS, fonts over HTTP)
// is loaded downgraded or blocked depending on the browser. Both should be served over https.
// Only https:// pages are analyzed - mixed content is a concern only on secure pages.

use std::collections::HashMap;
use std::time::Instant;

use regex::Regex;

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

const ANALYSIS_MIXED_CONTENT: &str = "Mixed content";

pub struct MixedContentAnalyzer {
    base: BaseAnalyzer,
    stats: AnalyzerStats,
    pages_with_active: usize,
    pages_with_passive: usize,
    https_pages_checked: usize,
}

impl MixedContentAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
            stats: AnalyzerStats::new(),
            pages_with_active: 0,
            pages_with_passive: 0,
            https_pages_checked: 0,
        }
    }

    /// Counts how many times `re` matches in `html` (non-overlapping, sequential).
    fn count_re(re: &Regex, html: &str) -> usize {
        let mut n = 0usize;
        let mut pos = 0usize;
        while let Some(m) = re.find(&html[pos..]) {
            n += 1;
            pos += m.end();
        }
        n
    }

    /// Counts (active, passive) http:// mixed-content references in `html`.
    fn count_mixed(html: &str) -> (usize, usize) {
        let active_patterns = [
            r#"<script[^>]+src\s*=\s*["']\s*http://"#,
            r#"<iframe[^>]+src\s*=\s*["']\s*http://"#,
            r#"<form[^>]+action\s*=\s*["']\s*http://"#,
            r#"<object[^>]+data\s*=\s*["']\s*http://"#,
            r#"<embed[^>]+src\s*=\s*["']\s*http://"#,
        ];
        let mut active = 0usize;
        for p in active_patterns {
            if let Ok(re) = Regex::new(p) {
                active += Self::count_re(&re, html);
            }
        }

        let passive_patterns = [
            r#"<img[^>]+src\s*=\s*["']\s*http://"#,
            r#"<link[^>]+href\s*=\s*["']\s*http://"#,
            r#"<source[^>]+src\s*=\s*["']\s*http://"#,
            r#"url\(\s*["']?\s*http://"#,
            r#"@import\s+["']\s*http://"#,
        ];
        let mut passive = 0usize;
        for p in passive_patterns {
            if let Ok(re) = Regex::new(p) {
                passive += Self::count_re(&re, html);
            }
        }

        (active, passive)
    }

    fn set_findings_to_summary(&self, status: &Status) {
        if self.pages_with_active > 0 {
            status.add_warning_to_summary(
                "mixed-content",
                &format!(
                    "{} page(s) with active mixed content (http:// scripts/iframes/forms on an https page)",
                    self.pages_with_active
                ),
            );
        } else if self.pages_with_passive > 0 {
            status.add_notice_to_summary(
                "mixed-content",
                &format!(
                    "{} page(s) with passive mixed content (http:// images/CSS/fonts on an https page)",
                    self.pages_with_passive
                ),
            );
        } else {
            status.add_ok_to_summary(
                "mixed-content",
                &format!(
                    "No mixed content detected across {} https page(s)",
                    self.https_pages_checked
                ),
            );
        }
    }
}

impl Default for MixedContentAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for MixedContentAnalyzer {
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
            "mixed-content".to_string(),
            "Mixed content".to_string(),
            "Nothing to report.".to_string(),
            columns,
            true,
            None,
            "ASC".to_string(),
            Some("Per-page http:// resource references embedded in https:// pages (active = scripts/iframes/forms, passive = images/CSS/fonts).".to_string()),
            None,
            None,
        );
        super_table.set_data(data);
        status.configure_super_table_url_stripping(&mut super_table);
        output.add_super_table(&super_table);
        status.add_super_table_at_end(super_table);

        self.set_findings_to_summary(status);
        self.base.measure_exec_time("MixedContentAnalyzer", "analyze", s);
    }

    fn analyze_visited_url(
        &mut self,
        visited_url: &VisitedUrl,
        body: Option<&str>,
        _headers: Option<&HashMap<String, String>>,
    ) -> Option<UrlAnalysisResult> {
        // Mixed content only applies to pages served over https.
        let is_https_html = visited_url.content_type == ContentTypeId::Html
            && visited_url.status_code == 200
            && visited_url.url.starts_with("https://")
            && body.is_some();
        if !is_https_html {
            return None;
        }
        let html = body?;
        let mut result = UrlAnalysisResult::new();
        self.https_pages_checked += 1;

        let (active, passive) = Self::count_mixed(html);

        if active > 0 {
            let msg = format!(
                "{} active mixed-content reference(s) (http:// scripts/iframes/forms on an https page)",
                active
            );
            result.add_warning(msg.clone(), ANALYSIS_MIXED_CONTENT, Some(vec![msg.clone()]));
            self.stats.add_warning(ANALYSIS_MIXED_CONTENT, Some(&visited_url.url));
            self.pages_with_active += 1;
        }
        if passive > 0 {
            let msg = format!(
                "{} passive mixed-content reference(s) (http:// images/CSS/fonts on an https page)",
                passive
            );
            result.add_notice(msg.clone(), ANALYSIS_MIXED_CONTENT, Some(vec![msg]));
            self.stats.add_notice(ANALYSIS_MIXED_CONTENT, Some(&visited_url.url));
            self.pages_with_passive += 1;
        }
        if active == 0 && passive == 0 {
            result.add_ok(
                "No mixed content (all referenced resources use https or are relative)".to_string(),
                ANALYSIS_MIXED_CONTENT,
                None,
            );
            self.stats.add_ok(ANALYSIS_MIXED_CONTENT, Some(&visited_url.url));
        }

        Some(result)
    }

    fn show_analyzed_visited_url_result_as_column(&self) -> Option<ExtraColumn> {
        ExtraColumn::new("Mixed".to_string(), Some(8), false, None, None, None).ok()
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        126
    }

    fn get_name(&self) -> &str {
        "MixedContentAnalyzer"
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
    fn detects_active_and_passive() {
        let html = r#"
        <html><body>
          <script src="http://ads.example.com/t.js"></script>
          <iframe src="http://widget.example.com/"></iframe>
          <img src="http://img.example.com/pic.jpg" alt="x">
          <img src="https://ok.example.com/pic.jpg" alt="y">
          <a href="http://insecure.example.com/"></a>
        </body></html>"#;
        let (active, passive) = MixedContentAnalyzer::count_mixed(html);
        assert_eq!(active, 2); // script + iframe
        assert_eq!(passive, 1); // one http img (link href is not counted as a resource)
    }

    #[test]
    fn clean_page() {
        let html = "<html><body><img src='https://ok.example.com/a.png'><a href='https://ok/'></a></body></html>";
        let (active, passive) = MixedContentAnalyzer::count_mixed(html);
        assert_eq!(active, 0);
        assert_eq!(passive, 0);
    }
}
