// SiteOne Crawler - BrokenAssetsAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Reports static assets (images, scripts, stylesheets, fonts, media) that failed to
// load: HTTP 4xx/5xx responses or a transport-level error (connection failed, timeout,
// ...). This is the complement of the page-404 analyzer: it focuses on the resources a
// visitor actually sees break (a missing logo, a dead JS bundle, a failed font) rather
// than on HTML documents.

use std::collections::HashMap;
use std::time::Instant;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::VisitedUrl;
use crate::utils;

const SUPER_TABLE_BROKEN_ASSETS: &str = "broken-assets";

pub struct BrokenAssetsAnalyzer {
    base: BaseAnalyzer,
}

impl BrokenAssetsAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
        }
    }

    /// A "broken asset" is a static resource (not an HTML document) that failed to load.
    ///
    /// A 4xx/5xx error page reports the error document's content type (often HTML), not the
    /// asset's real type, so the URL extension is the reliable signal for "is this an asset?".
    fn is_broken_asset(u: &VisitedUrl) -> bool {
        let is_error = u.status_code >= 400 || u.status_code < 0;
        is_error && Self::is_asset_url(&u.url)
    }

    /// True when the URL path ends in a static-asset extension (image/font/style/script/media/…).
    fn is_asset_url(url: &str) -> bool {
        let ext = Self::url_ext(url);
        matches!(
            ext.as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "ico" | "bmp" | "tiff")
                | Some("woff" | "woff2" | "ttf" | "otf" | "eot")
                | Some("css" | "js" | "mjs" | "map")
                | Some("mp3" | "mp4" | "webm" | "ogg" | "wav" | "aac")
                | Some("pdf" | "wasm" | "json")
        )
    }

    /// The last path segment's file extension (lower-cased), ignoring query/fragment, or None.
    fn url_ext(url: &str) -> Option<String> {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let file = path.rsplit('/').next().unwrap_or(path);
        let dot = file.rfind('.')?;
        let ext = &file[dot + 1..];
        if ext.is_empty() { None } else { Some(ext.to_lowercase()) }
    }

    fn asset_label(u: &VisitedUrl) -> String {
        let ext = Self::url_ext(&u.url);
        match ext.as_deref() {
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "ico" | "bmp" | "tiff") => {
                "Image".to_string()
            }
            Some("woff" | "woff2" | "ttf" | "otf" | "eot") => "Font".to_string(),
            Some("css") => "Stylesheet".to_string(),
            Some("js" | "mjs" | "map") => "Script".to_string(),
            Some("mp3" | "mp4" | "webm" | "ogg" | "wav" | "aac") => "Media".to_string(),
            _ => "Asset".to_string(),
        }
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

impl Default for BrokenAssetsAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for BrokenAssetsAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

        let visited = status.get_visited_urls();
        let mut broken: Vec<&VisitedUrl> = visited.iter().filter(|u| Self::is_broken_asset(u)).collect();
        broken.sort_by_key(|u| (u.status_code, u.url.clone()));

        let mut data: Vec<HashMap<String, String>> = Vec::new();
        for u in &broken {
            let mut row = HashMap::new();
            row.insert("asset".to_string(), u.url.clone());
            row.insert("type".to_string(), Self::asset_label(u));
            row.insert("status".to_string(), Self::status_label(u.status_code));
            row.insert(
                "source".to_string(),
                status.get_url_by_uq_id(&u.source_uq_id).unwrap_or_default(),
            );
            data.push(row);
        }

        let console_width = utils::get_console_width();
        let asset_width = (console_width as i32 - 30).clamp(30, 200);
        let source_width = (console_width as i32 - 48).clamp(30, 200);

        let columns = vec![
            SuperTableColumn::new(
                "asset".to_string(),
                "Asset".to_string(),
                asset_width,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "type".to_string(),
                "Type".to_string(),
                12,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "status".to_string(),
                "Status".to_string(),
                18,
                Some(Box::new(|value: &str, _render_into: &str| {
                    utils::get_color_text(value, "red", true)
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
                "Referenced by".to_string(),
                source_width,
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
            SUPER_TABLE_BROKEN_ASSETS.to_string(),
            "Broken assets".to_string(),
            "No broken assets detected.".to_string(),
            columns,
            true,
            Some("status".to_string()),
            "ASC".to_string(),
            None,
            None,
            None,
        );
        super_table.set_data(data);
        status.configure_super_table_url_stripping(&mut super_table);
        output.add_super_table(&super_table);
        status.add_super_table_at_beginning(super_table);

        let count = broken.len();
        if count == 0 {
            status.add_ok_to_summary(
                "broken-assets",
                "No broken assets (4xx/5xx or connection errors) were detected.",
            );
        } else {
            status.add_summary_item_by_ranges(
                "broken-assets",
                count as f64,
                &[(0.0, 0.0), (1.0, 10.0), (11.0, 50.0), (51.0, 100_000.0)],
                &[
                    "0 broken assets detected",
                    "{} broken asset(s) failed to load (4xx/5xx or connection error)",
                    "{} broken assets failed to load (4xx/5xx or connection error)",
                    "{} broken assets failed to load (4xx/5xx or connection error)",
                ],
            );
        }

        self.base.measure_exec_time("BrokenAssetsAnalyzer", "analyze", s);
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        121
    }

    fn get_name(&self) -> &str {
        "BrokenAssetsAnalyzer"
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
    fn broken_asset_detection() {
        let ok_image = VisitedUrl::new(
            "a".into(),
            "".into(),
            10,
            "https://ex.com/img.png".into(),
            200,
            0.1,
            Some(1024),
            ContentTypeId::Image,
            Some("image/png".into()),
            None,
            None,
            false,
            true,
            0,
            None,
        );
        let failed_script = VisitedUrl::new(
            "b".into(),
            "a".into(),
            30,
            "https://ex.com/app.js".into(),
            404,
            0.1,
            None,
            ContentTypeId::Script,
            Some("application/javascript".into()),
            None,
            None,
            false,
            true,
            0,
            None,
        );
        let connection_fail_font = VisitedUrl::new(
            "c".into(),
            "a".into(),
            60,
            "https://ex.com/font.woff2".into(),
            -1,
            0.1,
            None,
            ContentTypeId::Font,
            None,
            None,
            None,
            false,
            true,
            0,
            None,
        );
        let html_404 = VisitedUrl::new(
            "d".into(),
            "a".into(),
            10,
            "https://ex.com/missing".into(),
            404,
            0.1,
            Some(10),
            ContentTypeId::Html,
            Some("text/html".into()),
            None,
            None,
            false,
            true,
            0,
            None,
        );
        // The real-world case: a 404 image whose error page reports content-type text/html.
        let broken_image_reported_html = VisitedUrl::new(
            "e".into(),
            "a".into(),
            10,
            "https://ex.com/img/missing.png".into(),
            404,
            0.1,
            Some(10),
            ContentTypeId::Html,
            Some("text/html".into()),
            None,
            None,
            false,
            true,
            0,
            None,
        );

        assert!(!BrokenAssetsAnalyzer::is_broken_asset(&ok_image));
        assert!(BrokenAssetsAnalyzer::is_broken_asset(&failed_script));
        assert!(BrokenAssetsAnalyzer::is_broken_asset(&connection_fail_font));
        // A 404 HTML page is the page-404 analyzer's job, not this one's.
        assert!(!BrokenAssetsAnalyzer::is_broken_asset(&html_404));
        // A failed .png is a broken asset even though the error page's content-type is HTML.
        assert!(BrokenAssetsAnalyzer::is_broken_asset(&broken_image_reported_html));
    }

    #[test]
    fn asset_type_labels() {
        let u = |url: &str| {
            VisitedUrl::new(
                "x".into(),
                "a".into(),
                10,
                url.into(),
                404,
                0.1,
                None,
                ContentTypeId::Html,
                None,
                None,
                None,
                false,
                true,
                0,
                None,
            )
        };
        assert_eq!(BrokenAssetsAnalyzer::asset_label(&u("https://ex.com/a.png")), "Image");
        assert_eq!(
            BrokenAssetsAnalyzer::asset_label(&u("https://ex.com/a.woff2?v=2")),
            "Font"
        );
        assert_eq!(
            BrokenAssetsAnalyzer::asset_label(&u("https://ex.com/a.css")),
            "Stylesheet"
        );
        assert_eq!(BrokenAssetsAnalyzer::asset_label(&u("https://ex.com/a.js")), "Script");
        assert_eq!(BrokenAssetsAnalyzer::asset_label(&u("https://ex.com/page")), "Asset");
    }

    #[test]
    fn status_labels() {
        assert_eq!(BrokenAssetsAnalyzer::status_label(404), "404");
        assert_eq!(BrokenAssetsAnalyzer::status_label(-1), "Connection failed");
        assert_eq!(BrokenAssetsAnalyzer::status_label(-2), "Timeout");
    }
}
