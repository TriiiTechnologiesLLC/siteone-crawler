// SiteOne Crawler - RenderBlockingAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Detects render-blocking resources in a page's <head>: external stylesheets (<link
// rel="stylesheet">) and scripts loaded synchronously (<script src> without `defer`/`async`).
// These delay first paint because the browser must fetch and (for scripts) execute them before
// continuing to parse the document. A large number of such resources in the head is a well-known
// cause of a slow Time-to-First-Paint.

use std::collections::HashMap;
use std::time::Instant;

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

const ANALYSIS_RENDER_BLOCKING: &str = "Render-blocking resources";
const HEAVY_BLOCKING_THRESHOLD: usize = 4;

pub struct RenderBlockingAnalyzer {
    base: BaseAnalyzer,
    stats: AnalyzerStats,
    pages_with_blocking: usize,
    pages_heavily_blocking: usize,
}

impl RenderBlockingAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
            stats: AnalyzerStats::new(),
            pages_with_blocking: 0,
            pages_heavily_blocking: 0,
        }
    }

    /// Returns (blocking_css, blocking_js) counts found in the <head> of `html`.
    fn count_blocking(html: &str) -> (usize, usize) {
        let doc = scraper::Html::parse_document(html);

        let mut blocking_css = 0usize;
        if let Ok(css_sel) = Selector::parse("head link[href]") {
            for link in doc.select(&css_sel) {
                if let Some(rel) = link.value().attr("rel") {
                    let rels: Vec<String> = rel.split_whitespace().map(|s| s.to_lowercase()).collect();
                    if rels.contains(&"stylesheet".to_string()) {
                        blocking_css += 1;
                    }
                }
            }
        }

        let mut blocking_js = 0usize;
        if let Ok(js_sel) = Selector::parse("head script[src]") {
            for script in doc.select(&js_sel) {
                let has_async = script.value().attr("async").is_some();
                let has_defer = script.value().attr("defer").is_some();
                if !has_async && !has_defer {
                    blocking_js += 1;
                }
            }
        }

        (blocking_css, blocking_js)
    }

    fn set_findings_to_summary(&self, status: &Status) {
        if self.pages_heavily_blocking > 0 {
            status.add_warning_to_summary(
                "render-blocking",
                &format!(
                    "{} page(s) with {}+ render-blocking resources in <head> (consider defer/async or inlining critical CSS)",
                    self.pages_heavily_blocking, HEAVY_BLOCKING_THRESHOLD
                ),
            );
        } else if self.pages_with_blocking > 0 {
            status.add_notice_to_summary(
                "render-blocking",
                &format!(
                    "{} page(s) with render-blocking resources in <head>",
                    self.pages_with_blocking
                ),
            );
        } else {
            status.add_ok_to_summary("render-blocking", "No render-blocking resources found in page <head>");
        }
    }
}

impl Default for RenderBlockingAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for RenderBlockingAnalyzer {
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
            "render-blocking".to_string(),
            "Render blocking".to_string(),
            "Nothing to report.".to_string(),
            columns,
            true,
            None,
            "ASC".to_string(),
            Some(
                "Per-page count of render-blocking resources (stylesheets and synchronous scripts) in the <head>."
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
        self.base.measure_exec_time("RenderBlockingAnalyzer", "analyze", s);
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
        let mut result = UrlAnalysisResult::new();

        let (css, js) = Self::count_blocking(html);
        let total = css + js;

        if total == 0 {
            result.add_ok(
                "No render-blocking resources in <head>".to_string(),
                ANALYSIS_RENDER_BLOCKING,
                None,
            );
            self.stats.add_ok(ANALYSIS_RENDER_BLOCKING, Some(&visited_url.url));
        } else if total < HEAVY_BLOCKING_THRESHOLD {
            let msg = format!(
                "{} render-blocking resource(s) in <head> ({} stylesheet(s), {} script(s))",
                total, css, js
            );
            result.add_notice(msg.clone(), ANALYSIS_RENDER_BLOCKING, Some(vec![msg]));
            self.stats.add_notice(ANALYSIS_RENDER_BLOCKING, Some(&visited_url.url));
        } else {
            let msg = format!(
                "{} render-blocking resources in <head> ({} stylesheet(s), {} script(s)) - consider defer/async or inlining critical CSS",
                total, css, js
            );
            result.add_warning(msg.clone(), ANALYSIS_RENDER_BLOCKING, Some(vec![msg]));
            self.stats.add_warning(ANALYSIS_RENDER_BLOCKING, Some(&visited_url.url));
        }

        if total > 0 {
            self.pages_with_blocking += 1;
        }
        if total >= HEAVY_BLOCKING_THRESHOLD {
            self.pages_heavily_blocking += 1;
        }

        Some(result)
    }

    fn show_analyzed_visited_url_result_as_column(&self) -> Option<ExtraColumn> {
        ExtraColumn::new("Render".to_string(), Some(8), false, None, None, None).ok()
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        125
    }

    fn get_name(&self) -> &str {
        "RenderBlockingAnalyzer"
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
    fn counts_render_blocking_in_head() {
        let html = r#"
        <html><head>
          <link rel="stylesheet" href="/a.css">
          <link rel="stylesheet" href="/b.css">
          <link rel="preload" href="/c.woff2" as="font">
          <script src="/blocking.js"></script>
          <script src="/ok.js" defer></script>
          <script src="/ok2.js" async></script>
          <script>var x=1;</script>
        </head><body>
          <script src="/not-head.js"></script>
        </body></html>"#;
        let (css, js) = RenderBlockingAnalyzer::count_blocking(html);
        assert_eq!(css, 2);
        assert_eq!(js, 1);
    }

    #[test]
    fn ignores_non_head_and_lazy() {
        let html = "<html><body><head><script src='/a.js'></script></body></html>";
        let (_css, js) = RenderBlockingAnalyzer::count_blocking(html);
        assert_eq!(js, 0); // script is not in a real <head>
    }
}
