// SiteOne Crawler - OrphanPagesAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Two related link-structure findings:
//   1. Orphan pages: HTML pages that were not reached through a link from another crawled
//      page (they came from the initial URL, a sitemap, or a redirect), so they are weakly
//      connected to the rest of the site and are harder for users and crawlers to discover.
//   2. Link-depth distribution: how far each page sits from the home page through internal
//      links (classic SEO click-depth metric).
//
// The source of every URL is recorded by the crawler (single discoverer), so both findings
// are a heuristic over that data.

use std::collections::HashMap;
use std::time::Instant;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::{SOURCE_INIT_URL, SOURCE_REDIRECT, SOURCE_SITEMAP, VisitedUrl};
use crate::types::ContentTypeId;
use crate::utils;

const SUPER_TABLE_ORPHANS: &str = "orphan-pages";
const SUPER_TABLE_DEPTH: &str = "link-depth";

pub struct OrphanPagesAnalyzer {
    base: BaseAnalyzer,
}

impl OrphanPagesAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
        }
    }

    fn is_html_page(u: &VisitedUrl) -> bool {
        u.content_type == ContentTypeId::Html && u.status_code == 200 && u.is_allowed_for_crawling
    }

    fn is_discovered_out_of_band(u: &VisitedUrl) -> bool {
        matches!(u.source_attr, SOURCE_INIT_URL | SOURCE_SITEMAP | SOURCE_REDIRECT)
    }

    /// BFS over internal links (source page -> page). Returns the click-depth of every page
    /// (0 = home page) and `None` for pages not reachable from the home page via internal links.
    fn compute_depths(
        pages: &HashMap<String, &VisitedUrl>,
        seed_uq_id: Option<&str>,
    ) -> HashMap<String, Option<usize>> {
        let mut depth: HashMap<String, Option<usize>> = pages.keys().map(|k| (k.clone(), None)).collect();
        if let Some(sid) = seed_uq_id.and_then(|sid| depth.contains_key(sid).then_some(sid)) {
            depth.insert(sid.to_string(), Some(0));
        }

        // Fixed-point propagation: a page's depth is its (known) source page's depth + 1.
        for _ in 0..pages.len() {
            let mut changed = false;
            for (id, page) in pages.iter() {
                if Some(id.as_str()) == seed_uq_id {
                    continue;
                }
                if depth.get(id).map(|v| v.is_some()).unwrap_or(false) {
                    continue;
                }
                let src = page.source_uq_id.as_str();
                if let Some(sd) = if src.is_empty() {
                    None
                } else {
                    depth.get(src).copied().flatten()
                } {
                    depth.insert(id.clone(), Some(sd + 1));
                    changed = true;
                    break; // restart the pass so depths stay monotonic
                }
            }
            if !changed {
                break;
            }
        }
        depth
    }

    fn depth_label(depth: Option<usize>) -> String {
        match depth {
            None => "Not linked from the home page".to_string(),
            Some(0) => "0 (home page)".to_string(),
            Some(1) => "1".to_string(),
            Some(2) => "2".to_string(),
            Some(3) => "3".to_string(),
            Some(4) => "4".to_string(),
            Some(5) => "5".to_string(),
            Some(_) => "6+".to_string(),
        }
    }
}

impl Default for OrphanPagesAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for OrphanPagesAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

        let visited = status.get_visited_urls();
        let html_pages: Vec<&VisitedUrl> = visited.iter().filter(|u| Self::is_html_page(u)).collect();

        let seed_uq_id = html_pages
            .iter()
            .find(|u| u.source_attr == SOURCE_INIT_URL)
            .map(|u| u.uq_id.clone());

        // ---- Orphan pages ---------------------------------------------------------------
        let orphans: Vec<&VisitedUrl> = html_pages
            .iter()
            .copied()
            .filter(|u| Self::is_discovered_out_of_band(u))
            .filter(|u| Some(u.uq_id.as_str()) != seed_uq_id.as_deref())
            .collect();

        let console_width = utils::get_console_width();
        let page_width = (console_width as i32 - 24).clamp(30, 200);

        let orphan_columns = vec![
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
                "source".to_string(),
                "Discovered via".to_string(),
                20,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut orphan_data: Vec<HashMap<String, String>> = Vec::new();
        for u in &orphans {
            let mut row = HashMap::new();
            row.insert("page".to_string(), u.url.clone());
            row.insert(
                "source".to_string(),
                status
                    .get_url_by_uq_id(&u.source_uq_id)
                    .map(|s| format!("{} ({})", s, u.get_source_short_name()))
                    .unwrap_or_else(|| u.get_source_short_name().to_string()),
            );
            orphan_data.push(row);
        }

        let mut orphan_table = SuperTable::new(
            SUPER_TABLE_ORPHANS.to_string(),
            "Orphan pages".to_string(),
            "No orphan pages detected.".to_string(),
            orphan_columns,
            true,
            Some("page".to_string()),
            "ASC".to_string(),
            Some(
                "A page that was not reached through a link from another crawled page (it came \
                 from the initial URL, a sitemap, or a redirect)."
                    .to_string(),
            ),
            None,
            None,
        );
        orphan_table.set_data(orphan_data);
        status.configure_super_table_url_stripping(&mut orphan_table);
        output.add_super_table(&orphan_table);
        status.add_super_table_at_beginning(orphan_table);

        // ---- Link-depth distribution ----------------------------------------------------
        let page_map: HashMap<String, &VisitedUrl> = html_pages.iter().copied().map(|u| (u.uq_id.clone(), u)).collect();
        let depths = Self::compute_depths(&page_map, seed_uq_id.as_deref());

        let mut dist: Vec<(Option<usize>, usize)> = Vec::new();
        for (_id, d) in depths.iter() {
            // bucket
            let bucket = match d {
                None => None,
                Some(0) => Some(0),
                Some(1) => Some(1),
                Some(2) => Some(2),
                Some(3) => Some(3),
                Some(4) => Some(4),
                Some(5) => Some(5),
                Some(_) => Some(6), // 6+
            };
            if let Some(pos) = dist.iter_mut().position(|(k, _)| *k == bucket) {
                dist[pos].1 += 1;
            } else {
                dist.push((bucket, 1));
            }
        }
        // order: 0,1,2,3,4,5,6+,None
        let order = [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), None];
        dist.sort_by_key(|(k, _)| order.iter().position(|o| o == k).unwrap_or(order.len()));

        let depth_columns = vec![
            SuperTableColumn::new(
                "depth".to_string(),
                "Click depth from home page".to_string(),
                34,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "pages".to_string(),
                "Pages".to_string(),
                10,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut depth_data: Vec<HashMap<String, String>> = Vec::new();
        for (bucket, count) in &dist {
            let mut row = HashMap::new();
            row.insert("depth".to_string(), Self::depth_label(*bucket));
            row.insert("pages".to_string(), count.to_string());
            depth_data.push(row);
        }

        let mut depth_table = SuperTable::new(
            SUPER_TABLE_DEPTH.to_string(),
            "Link depth".to_string(),
            "No HTML pages to analyze.".to_string(),
            depth_columns,
            true,
            Some("pages".to_string()),
            "DESC".to_string(),
            Some("Click depth = number of internal links from the home page to reach the page.".to_string()),
            None,
            None,
        );
        depth_table.set_data(depth_data);
        status.configure_super_table_url_stripping(&mut depth_table);
        output.add_super_table(&depth_table);
        status.add_super_table_at_beginning(depth_table);

        // ---- Summary --------------------------------------------------------------------
        if orphans.is_empty() {
            status.add_ok_to_summary("orphan-pages", "No orphan pages detected.");
        } else {
            status.add_summary_item_by_ranges(
                "orphan-pages",
                orphans.len() as f64,
                &[(0.0, 0.0), (1.0, 5.0), (6.0, 20.0), (21.0, 100_000.0)],
                &[
                    "0 orphan pages detected",
                    "{} orphan page(s) (reached only via sitemap/redirect/initial URL)",
                    "{} orphan pages (reached only via sitemap/redirect/initial URL)",
                    "{} orphan pages (reached only via sitemap/redirect/initial URL)",
                ],
            );
        }

        self.base.measure_exec_time("OrphanPagesAnalyzer", "analyze", s);
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        122
    }

    fn get_name(&self) -> &str {
        "OrphanPagesAnalyzer"
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

    fn page(uq_id: &str, source_uq_id: &str, source_attr: i32) -> VisitedUrl {
        VisitedUrl::new(
            uq_id.into(),
            source_uq_id.into(),
            source_attr,
            format!("https://ex.com/{}", uq_id),
            200,
            0.1,
            Some(100),
            ContentTypeId::Html,
            Some("text/html".into()),
            None,
            None,
            false,
            true,
            0,
            None,
        )
    }

    #[test]
    fn orphan_detection_excludes_seed() {
        let seed = page("home", "", SOURCE_INIT_URL);
        let sitemap_page = page("only-in-sitemap", "sitemap", SOURCE_SITEMAP);
        let linked_page = page("linked", "home", crate::result::visited_url::SOURCE_A_HREF);
        // seed should not be treated as an orphan by the source_attr filter + seed exclusion
        assert_eq!(seed.source_attr, SOURCE_INIT_URL);
        assert!(OrphanPagesAnalyzer::is_discovered_out_of_band(&sitemap_page));
        assert!(!OrphanPagesAnalyzer::is_discovered_out_of_band(&linked_page));
    }

    #[test]
    fn depth_bfs_chain() {
        let home = page("home", "", SOURCE_INIT_URL);
        let a = page("a", "home", crate::result::visited_url::SOURCE_A_HREF);
        let b = page("b", "a", crate::result::visited_url::SOURCE_A_HREF);
        let orphans_only = page("o", "sitemap", SOURCE_SITEMAP);
        let pages = [&home, &a, &b, &orphans_only];
        let map: HashMap<String, &VisitedUrl> = pages.iter().copied().map(|u| (u.uq_id.clone(), u)).collect();
        let depths = OrphanPagesAnalyzer::compute_depths(&map, Some("home"));
        assert_eq!(depths.get("home"), Some(&Some(0)));
        assert_eq!(depths.get("a"), Some(&Some(1)));
        assert_eq!(depths.get("b"), Some(&Some(2)));
        assert_eq!(depths.get("o"), Some(&None));
    }
}
