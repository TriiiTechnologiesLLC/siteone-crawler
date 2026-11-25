// SiteOne Crawler - SitemapCoverageAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Reports the sitemap -> crawl relationship: which URLs were discovered through a sitemap,
// whether they loaded, and (when a sitemap was not used at all) that the site appears not to
// expose one. Sites without a sitemap are harder for search engines and AI crawlers to index
// exhaustively, and sitemap URLs that 4xx/5xx are a concrete gap between "advertised" and
// "reachable".

use std::collections::HashMap;
use std::time::Instant;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::{SOURCE_SITEMAP, VisitedUrl};
use crate::utils;

const SUPER_TABLE_SITEMAP: &str = "sitemap-coverage";

pub struct SitemapCoverageAnalyzer {
    base: BaseAnalyzer,
}

impl SitemapCoverageAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
        }
    }

    fn is_from_sitemap(u: &VisitedUrl) -> bool {
        u.source_attr == SOURCE_SITEMAP
    }

    fn status_label(code: i32) -> String {
        match code {
            -1 => "Connection failed".to_string(),
            -2 => "Timeout".to_string(),
            -3 => "Server reset".to_string(),
            -4 => "Send error".to_string(),
            -6 => "Skipped".to_string(),
            c if c >= 100 => c.to_string(),
            c => c.to_string(),
        }
    }
}

impl Default for SitemapCoverageAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for SitemapCoverageAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

        let visited = status.get_visited_urls();
        let mut sitemap_urls: Vec<&VisitedUrl> = visited.iter().filter(|u| Self::is_from_sitemap(u)).collect();
        sitemap_urls.sort_by(|a, b| a.url.cmp(&b.url));

        let console_width = utils::get_console_width();
        let url_width = (console_width as i32 - 40).clamp(30, 200);

        let columns = vec![
            SuperTableColumn::new(
                "url".to_string(),
                "Sitemap URL".to_string(),
                url_width,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "status".to_string(),
                "Status".to_string(),
                18,
                Some(Box::new(|value: &str, _r: &str| {
                    let bad = value.parse::<i32>().map(|c| !(0..400).contains(&c)).unwrap_or(true);
                    if bad {
                        utils::get_color_text(value, "red", true)
                    } else {
                        utils::get_color_text(value, "green", true)
                    }
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "source".to_string(),
                "Sitemap file".to_string(),
                (console_width as i32 - 60).clamp(24, 160),
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut data: Vec<HashMap<String, String>> = Vec::new();
        for u in &sitemap_urls {
            let mut row = HashMap::new();
            row.insert("url".to_string(), u.url.clone());
            row.insert("status".to_string(), Self::status_label(u.status_code));
            row.insert(
                "source".to_string(),
                status.get_url_by_uq_id(&u.source_uq_id).unwrap_or_default(),
            );
            data.push(row);
        }

        let mut super_table = SuperTable::new(
            SUPER_TABLE_SITEMAP.to_string(),
            "Sitemap coverage".to_string(),
            "No sitemap-sourced URLs were discovered.".to_string(),
            columns,
            true,
            Some("status".to_string()),
            "ASC".to_string(),
            Some("URLs discovered via a sitemap (XML/TXT) and their crawl result.".to_string()),
            None,
            None,
        );
        super_table.set_data(data);
        status.configure_super_table_url_stripping(&mut super_table);
        output.add_super_table(&super_table);
        status.add_super_table_at_beginning(super_table);

        // ---- Summary --------------------------------------------------------------------
        let total = sitemap_urls.len();
        let errors = sitemap_urls.iter().filter(|u| u.status_code != 200).count();

        if total == 0 {
            status.add_notice_to_summary(
                "sitemap-coverage",
                "No sitemap-sourced URLs were discovered (the site may not expose a sitemap).",
            );
        } else if errors == 0 {
            status.add_ok_to_summary(
                "sitemap-coverage",
                &format!("All {} sitemap-sourced URL(s) were successfully crawled.", total),
            );
        } else {
            status.add_summary_item_by_ranges(
                "sitemap-coverage",
                errors as f64,
                &[(0.0, 0.0), (1.0, 10.0), (11.0, 50.0), (51.0, 100_000.0)],
                &[
                    "0 sitemap-sourced URLs failed to load",
                    "{} of {} sitemap-sourced URL(s) failed to load",
                    "{} of {} sitemap-sourced URLs failed to load",
                    "{} of {} sitemap-sourced URLs failed to load",
                ],
            );
        }

        self.base.measure_exec_time("SitemapCoverageAnalyzer", "analyze", s);
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        123
    }

    fn get_name(&self) -> &str {
        "SitemapCoverageAnalyzer"
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
    use crate::types::ContentTypeId;

    #[test]
    fn sitemap_source_detection() {
        let from_sitemap = VisitedUrl::new(
            "a".into(),
            "sm".into(),
            SOURCE_SITEMAP,
            "https://ex.com/one".into(),
            200,
            0.1,
            Some(10),
            ContentTypeId::Html,
            None,
            None,
            None,
            false,
            true,
            0,
            None,
        );
        assert!(SitemapCoverageAnalyzer::is_from_sitemap(&from_sitemap));
    }
}
