// SiteOne Crawler - SitemapQualityAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Quality of the XML sitemap(s) used in the crawl — beyond mere reachability, which the
// sitemap-coverage analyzer already reports:
//   1. Kind & namespaces — URL list (<urlset>) vs. index (<sitemapindex>), and whether the
//      sitemap carries image:/video: namespace entries that Google can index as rich results.
//   2. lastmod accuracy — for each <url><lastmod> whose page was also crawled, compares the
//      sitemap's lastmod with the page's Last-Modified response header; a disagreement of more
//      than 30 days is a stale/misleading freshness signal.
//
// Deterministic; no network, no LLM. Reads the already-fetched sitemap document body and the
// crawled page headers from Status (no refetch).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use chrono::{DateTime, NaiveDate, Utc};
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::SOURCE_SITEMAP;

/// lastmod values disagreeing with the page's Last-Modified by more than this many days are flagged.
const LASTMOD_MAX_DIFF_DAYS: i64 = 30;

pub struct SitemapQualityAnalyzer {
    base: BaseAnalyzer,
}

impl SitemapQualityAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
        }
    }
}

impl Default for SitemapQualityAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct UrlEntry {
    loc: String,
    lastmod: Option<String>,
}

#[derive(Default)]
struct SitemapDoc {
    is_index: bool,
    has_images: bool,
    has_videos: bool,
    urls: Vec<UrlEntry>,
    children: Vec<String>,
}

/// The tag name of a quick-xml event as a &str ("" if not valid UTF-8).
fn tag(name: &[u8]) -> &str {
    std::str::from_utf8(name).unwrap_or("")
}

/// Minimal sitemap reader: detects the document kind, image:/video: namespaces, and collects
/// (loc, lastmod) for <urlset> or child <loc> for <sitemapindex>.
fn parse_sitemap(body: &str) -> SitemapDoc {
    let mut doc = SitemapDoc::default();
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut text = String::new();
    let mut in_url = false;
    let mut in_sitemap = false;
    let mut cur_loc: Option<String> = None;
    let mut cur_lastmod: Option<String> = None;

    #[derive(PartialEq)]
    enum Capture {
        None,
        Loc,
        Lastmod,
    }
    let mut capture = Capture::None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let full = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if full.starts_with("image:") {
                    doc.has_images = true;
                }
                if full.starts_with("video:") {
                    doc.has_videos = true;
                }
                if tag(e.local_name().as_ref()) == "sitemapindex" {
                    doc.is_index = true;
                }
                if tag(e.local_name().as_ref()) == "url" {
                    in_url = true;
                    cur_loc = None;
                    cur_lastmod = None;
                }
                if tag(e.local_name().as_ref()) == "sitemap" {
                    in_sitemap = true;
                    cur_loc = None;
                }
                if tag(e.local_name().as_ref()) == "loc" {
                    capture = Capture::Loc;
                    text.clear();
                }
                if tag(e.local_name().as_ref()) == "lastmod" {
                    capture = Capture::Lastmod;
                    text.clear();
                }
            }
            Ok(Event::Text(e)) => {
                if capture != Capture::None
                    && let Ok(t) = e.decode()
                {
                    text.push_str(&t);
                }
            }
            Ok(Event::End(e)) => {
                if tag(e.local_name().as_ref()) == "loc" {
                    let loc = text.trim().to_string();
                    text.clear();
                    if !loc.is_empty() {
                        if in_url {
                            cur_loc = Some(loc);
                        } else if in_sitemap {
                            doc.children.push(loc);
                        }
                    }
                    capture = Capture::None;
                } else if tag(e.local_name().as_ref()) == "lastmod" {
                    let lm = text.trim().to_string();
                    text.clear();
                    if !lm.is_empty() {
                        cur_lastmod = Some(lm);
                    }
                    capture = Capture::None;
                } else if tag(e.local_name().as_ref()) == "url" {
                    in_url = false;
                    if let Some(loc) = cur_loc.take() {
                        doc.urls.push(UrlEntry {
                            loc,
                            lastmod: cur_lastmod.take(),
                        });
                    }
                    cur_lastmod = None;
                } else if tag(e.local_name().as_ref()) == "sitemap" {
                    in_sitemap = false;
                    cur_loc = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    doc
}

/// Parse a sitemap lastmod (RFC3339 / date-only) or an HTTP Last-Modified date to a UTC instant.
fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    let v = value.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(v, "%Y-%m-%d")
        && let Some(naive) = d.and_hms_opt(0, 0, 0)
    {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(v) {
        return Some(dt.with_timezone(&Utc));
    }
    None
}

impl Analyzer for SitemapQualityAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let start = Instant::now();
        let visited = status.get_visited_urls();

        // The sitemap document(s) that seeded crawled pages.
        let mut sitemap_uqs: Vec<String> = Vec::new();
        for u in &visited {
            if u.source_attr == SOURCE_SITEMAP && !u.source_uq_id.is_empty() && !sitemap_uqs.contains(&u.source_uq_id) {
                sitemap_uqs.push(u.source_uq_id.clone());
            }
        }

        // url -> uq_id (raw + trailing-slash-normalized) for lastmod cross-checks.
        let mut by_url: HashMap<String, String> = HashMap::new();
        let mut crawled: HashSet<String> = HashSet::new();
        for u in &visited {
            by_url.entry(u.url.clone()).or_insert_with(|| u.uq_id.clone());
            by_url
                .entry(u.url.trim_end_matches('/').to_string())
                .or_insert_with(|| u.uq_id.clone());
            crawled.insert(u.url.clone());
            crawled.insert(u.url.trim_end_matches('/').to_string());
        }

        struct Row {
            url: String,
            kind: String,
            count: usize,
            images: bool,
            videos: bool,
            lastmod: String,
            note: String,
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut url_lists = 0usize;
        let mut indexes = 0usize;
        let mut total_urls = 0usize;
        let mut sitemaps_with_images = 0usize;
        let mut sitemaps_with_videos = 0usize;
        let mut lastmod_mismatches = 0usize;
        let mut index_children_missing = 0usize;

        for uq in &sitemap_uqs {
            let url = status.get_url_by_uq_id(uq).unwrap_or_else(|| uq.clone());
            let Some(body) = status.get_url_body_text(uq) else {
                rows.push(Row {
                    url,
                    kind: "unknown".to_string(),
                    count: 0,
                    images: false,
                    videos: false,
                    lastmod: "-".to_string(),
                    note: "sitemap body not stored".to_string(),
                });
                continue;
            };

            let doc = parse_sitemap(&body);
            if doc.has_images {
                sitemaps_with_images += 1;
            }
            if doc.has_videos {
                sitemaps_with_videos += 1;
            }

            if doc.is_index {
                indexes += 1;
                let missing = doc
                    .children
                    .iter()
                    .filter(|c| !crawled.contains(c.as_str()) && !crawled.contains(c.as_str().trim_end_matches('/')))
                    .count();
                index_children_missing += missing;
                rows.push(Row {
                    url,
                    kind: "index".to_string(),
                    count: doc.children.len(),
                    images: doc.has_images,
                    videos: doc.has_videos,
                    lastmod: "-".to_string(),
                    note: if missing > 0 {
                        format!("{missing} child sitemap(s) were not fetched")
                    } else {
                        "all child sitemaps fetched".to_string()
                    },
                });
            } else {
                url_lists += 1;
                total_urls += doc.urls.len();
                let with_lm = doc.urls.iter().filter(|e| e.lastmod.is_some()).count();
                let mut mismatches = 0usize;
                for entry in &doc.urls {
                    if let Some(lm) = &entry.lastmod {
                        let key = entry.loc.trim();
                        let page_uq = by_url
                            .get(key)
                            .cloned()
                            .or_else(|| by_url.get(entry.loc.trim_end_matches('/')).cloned());
                        if let Some(page_uq) = page_uq
                            && let Some(headers) = status.get_url_headers(&page_uq)
                            && let Some(lm_val) = headers
                                .iter()
                                .find(|(k, _)| k.eq_ignore_ascii_case("last-modified"))
                                .map(|(_, v)| v.clone())
                            && let (Some(a), Some(b)) = (parse_date(lm), parse_date(&lm_val))
                            && (a - b).num_days().abs() > LASTMOD_MAX_DIFF_DAYS
                        {
                            mismatches += 1;
                        }
                    }
                }
                lastmod_mismatches += mismatches;
                rows.push(Row {
                    url,
                    kind: "url list".to_string(),
                    count: doc.urls.len(),
                    images: doc.has_images,
                    videos: doc.has_videos,
                    lastmod: format!("{with_lm}/{}", doc.urls.len()),
                    note: if mismatches > 0 {
                        format!(
                            "{mismatches} lastmod value(s) disagree with the page's Last-Modified by more than {LASTMOD_MAX_DIFF_DAYS} days"
                        )
                    } else {
                        "lastmod consistent".to_string()
                    },
                });
            }
        }

        let data: Vec<HashMap<String, String>> = rows
            .iter()
            .map(|r| {
                let mut row: HashMap<String, String> = HashMap::new();
                row.insert("file".to_string(), r.url.clone());
                row.insert("kind".to_string(), r.kind.clone());
                row.insert("count".to_string(), r.count.to_string());
                row.insert(
                    "images".to_string(),
                    if r.images { "yes".to_string() } else { "no".to_string() },
                );
                row.insert(
                    "videos".to_string(),
                    if r.videos { "yes".to_string() } else { "no".to_string() },
                );
                row.insert("lastmod".to_string(), r.lastmod.clone());
                row.insert("note".to_string(), r.note.clone());
                row
            })
            .collect();

        let columns = vec![
            SuperTableColumn::new(
                "file".to_string(),
                "Sitemap".to_string(),
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
                "kind".to_string(),
                "Kind".to_string(),
                10,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "count".to_string(),
                "URLs / children".to_string(),
                15,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "images".to_string(),
                "Images".to_string(),
                8,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "videos".to_string(),
                "Videos".to_string(),
                8,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "lastmod".to_string(),
                "lastmod".to_string(),
                10,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "note".to_string(),
                "Notes".to_string(),
                -1,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut super_table = SuperTable::new(
            "sitemap-quality".to_string(),
            "Sitemap quality analysis".to_string(),
            "No sitemap was used in this crawl.".to_string(),
            columns,
            true,
            Some("file".to_string()),
            "ASC".to_string(),
            Some(
                "Quality of the XML sitemaps used in the crawl: URL-list vs. index, image:/video: namespaces, and lastmod accuracy against the crawled pages' Last-Modified headers."
                    .to_string(),
            ),
            None,
            None,
        );
        super_table.set_data(data);
        status.configure_super_table_url_stripping(&mut super_table);
        output.add_super_table(&super_table);
        status.add_super_table_at_end(super_table);

        let sitemaps = sitemap_uqs.len();
        if sitemaps == 0 {
            status.add_notice_to_summary(
                "sitemap-quality",
                "No sitemap was used in this crawl, so there was nothing to assess.",
            );
        } else {
            let positives = [
                format!("{url_lists} URL list(s), {indexes} index(es)"),
                format!("{total_urls} URLs total"),
                format!("{sitemaps_with_images} with image entries, {sitemaps_with_videos} with video entries"),
            ];
            let mut problems: Vec<String> = Vec::new();
            if lastmod_mismatches > 0 {
                problems.push(format!(
                    "{lastmod_mismatches} URL(s) with a stale lastmod (disagrees with Last-Modified by more than {LASTMOD_MAX_DIFF_DAYS} days)"
                ));
            }
            if index_children_missing > 0 {
                problems.push(format!(
                    "{index_children_missing} child sitemap(s) from an index were not fetched"
                ));
            }
            if problems.is_empty() {
                status.add_ok_to_summary(
                    "sitemap-quality",
                    &format!("Sitemap(s) look healthy ({}); lastmod consistent with the crawled pages' Last-Modified headers.", positives.join(", ")),
                );
            } else {
                status.add_warning_to_summary(
                    "sitemap-quality",
                    &format!("Sitemap(s): {}; {}", positives.join(", "), problems.join("; ")),
                );
            }
        }

        self.base.measure_exec_time("SitemapQualityAnalyzer", "analyze", start);
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        235
    }

    fn get_name(&self) -> &str {
        "SitemapQualityAnalyzer"
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
    fn parses_urlset_with_lastmod_and_image() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"
       xmlns:image="http://www.google.com/schemas/sitemap-image/1.0">
  <url><loc>https://example.com/a</loc><lastmod>2020-01-01T00:00:00+00:00</lastmod>
       <image:image><image:loc>https://example.com/a.jpg</image:loc></image:image></url>
  <url><loc>https://example.com/b</loc></url>
</urlset>"#;
        let d = parse_sitemap(xml);
        assert!(!d.is_index);
        assert_eq!(d.urls.len(), 2);
        assert!(d.urls[0].lastmod.is_some());
        assert!(d.urls[1].lastmod.is_none());
        assert!(d.has_images);
        assert!(!d.has_videos);
    }

    #[test]
    fn parses_index_children() {
        let xml = "<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\"><sitemap><loc>https://example.com/s1.xml</loc></sitemap><sitemap><loc>https://example.com/s2.xml</loc></sitemap></sitemapindex>";
        let d = parse_sitemap(xml);
        assert!(d.is_index);
        assert_eq!(d.children.len(), 2);
    }

    #[test]
    fn detects_video_namespace() {
        let xml = "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" xmlns:video=\"http://www.google.com/schemas/sitemap-video/1.0\"><url><loc>https://example.com/v</loc><video:video><video:title>x</video:title></video:video></url></urlset>";
        let d = parse_sitemap(xml);
        assert!(d.has_videos);
    }

    #[test]
    fn date_parse_and_staleness() {
        let old = parse_date("2020-01-01T00:00:00+00:00").expect("rfc3339");
        let new = parse_date("Fri, 03 Mar 2023 00:00:00 GMT").expect("http-date");
        assert!((old - new).num_days().abs() > LASTMOD_MAX_DIFF_DAYS);
        let nearby = parse_date("2020-01-15").expect("date-only");
        assert!((nearby - old).num_days().abs() <= LASTMOD_MAX_DIFF_DAYS);
    }
}
