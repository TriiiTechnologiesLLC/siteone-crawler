// SiteOne Crawler - ImageWeightAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Inventory and weight analysis of the images a site actually serves: how many there are,
// their total and average weight, the format mix (PNG/JPEG/WebP/AVIF/SVG/...), and which
// individual files are oversized (> 200 KB). Heavy, non-optimised images are one of the most
// common causes of slow page loads and high data usage on mobile.

use std::collections::HashMap;
use std::time::Instant;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::VisitedUrl;
use crate::types::ContentTypeId;
use crate::utils;

const SUPER_TABLE_IMAGES: &str = "image-inventory";
const SUPER_TABLE_FORMATS: &str = "image-formats";
const OVERSIZED_IMAGE_BYTES: i64 = 200 * 1024; // 200 KB
const MAX_ROWS: usize = 50;

pub struct ImageWeightAnalyzer {
    base: BaseAnalyzer,
}

impl ImageWeightAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
        }
    }

    fn is_crawled_image(u: &VisitedUrl) -> bool {
        u.status_code == 200 && u.content_type == ContentTypeId::Image
    }

    fn image_format(header: Option<&str>, url: &str) -> String {
        let h = header.unwrap_or("").to_lowercase();
        if h.contains("avif") {
            "AVIF".to_string()
        } else if h.contains("webp") {
            "WebP".to_string()
        } else if h.contains("svg") {
            "SVG".to_string()
        } else if h.contains("png") {
            "PNG".to_string()
        } else if h.contains("jpeg") || h.contains("jpg") {
            "JPEG".to_string()
        } else if h.contains("gif") {
            "GIF".to_string()
        } else if h.contains("bmp") {
            "BMP".to_string()
        } else if h.contains("tiff") || h.contains("tif") {
            "TIFF".to_string()
        } else if url.to_lowercase().ends_with(".ico") || h.contains("icon") {
            "ICO".to_string()
        } else {
            "Other".to_string()
        }
    }
}

impl Default for ImageWeightAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for ImageWeightAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

        let visited = status.get_visited_urls();
        let images: Vec<&VisitedUrl> = visited.iter().filter(|u| Self::is_crawled_image(u)).collect();

        let total_bytes: i64 = images.iter().map(|u| u.size.unwrap_or(0)).sum();
        let avg_bytes = if images.is_empty() {
            0
        } else {
            total_bytes / images.len() as i64
        };
        let oversized: Vec<&VisitedUrl> = images
            .iter()
            .copied()
            .filter(|u| u.size.unwrap_or(0) > OVERSIZED_IMAGE_BYTES)
            .collect();

        // ---- Image inventory (top by size) --------------------------------------------
        let console_width = utils::get_console_width();
        let url_width = (console_width as i32 - 44).clamp(30, 200);
        let mut sorted = images.clone();
        sorted.sort_by_key(|u| -(u.size.unwrap_or(0)));
        sorted.truncate(MAX_ROWS);

        let image_columns = vec![
            SuperTableColumn::new(
                "image".to_string(),
                "Image".to_string(),
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
                "format".to_string(),
                "Format".to_string(),
                8,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "size".to_string(),
                "Size".to_string(),
                12,
                Some(Box::new(|value: &str, _r: &str| {
                    let big = value.parse::<i64>().map(|b| b > OVERSIZED_IMAGE_BYTES).unwrap_or(false);
                    if big {
                        utils::get_color_text(value, "red", true)
                    } else {
                        value.to_string()
                    }
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut image_data: Vec<HashMap<String, String>> = Vec::new();
        for u in &sorted {
            let mut row = HashMap::new();
            row.insert("image".to_string(), u.url.clone());
            row.insert(
                "format".to_string(),
                Self::image_format(u.content_type_header.as_deref(), &u.url),
            );
            row.insert("size".to_string(), u.size.unwrap_or(0).to_string());
            image_data.push(row);
        }

        let mut image_table = SuperTable::new(
            SUPER_TABLE_IMAGES.to_string(),
            "Images".to_string(),
            "No images were crawled.".to_string(),
            image_columns,
            true,
            Some("size".to_string()),
            "DESC".to_string(),
            Some(format!(
                "Top {} of {} images by size. Oversized = > {} B.",
                MAX_ROWS,
                images.len(),
                OVERSIZED_IMAGE_BYTES
            )),
            None,
            None,
        );
        image_table.set_data(image_data);
        status.configure_super_table_url_stripping(&mut image_table);
        output.add_super_table(&image_table);
        status.add_super_table_at_beginning(image_table);

        // ---- Format distribution -------------------------------------------------------
        let mut formats: Vec<(String, usize, i64)> = Vec::new();
        for u in &images {
            let fmt = Self::image_format(u.content_type_header.as_deref(), &u.url);
            let bytes = u.size.unwrap_or(0);
            if let Some(pos) = formats.iter_mut().position(|(f, _, _)| *f == fmt) {
                formats[pos].1 += 1;
                formats[pos].2 += bytes;
            } else {
                formats.push((fmt, 1, bytes));
            }
        }
        formats.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));

        let format_columns = vec![
            SuperTableColumn::new(
                "format".to_string(),
                "Format".to_string(),
                10,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "count".to_string(),
                "Images".to_string(),
                8,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "bytes".to_string(),
                "Total size".to_string(),
                14,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        let mut format_data: Vec<HashMap<String, String>> = Vec::new();
        for (fmt, count, bytes) in &formats {
            let mut row = HashMap::new();
            row.insert("format".to_string(), fmt.clone());
            row.insert("count".to_string(), count.to_string());
            row.insert("bytes".to_string(), bytes.to_string());
            format_data.push(row);
        }

        let mut format_table = SuperTable::new(
            SUPER_TABLE_FORMATS.to_string(),
            "Image formats".to_string(),
            "No images were crawled.".to_string(),
            format_columns,
            true,
            Some("count".to_string()),
            "DESC".to_string(),
            Some("Format mix of the crawled images, inferred from the Content-Type header.".to_string()),
            None,
            None,
        );
        format_table.set_data(format_data);
        status.configure_super_table_url_stripping(&mut format_table);
        output.add_super_table(&format_table);
        status.add_super_table_at_beginning(format_table);

        // ---- Summary --------------------------------------------------------------------
        if images.is_empty() {
            status.add_ok_to_summary("image-weight", "No images were crawled.");
        } else {
            status.add_info_to_summary(
                "image-weight",
                &format!(
                    "{} image(s), {} total, {} average",
                    images.len(),
                    utils::get_formatted_size(total_bytes, 1),
                    utils::get_formatted_size(avg_bytes, 0),
                ),
            );

            if oversized.is_empty() {
                status.add_ok_to_summary(
                    "image-weight-oversized",
                    "No oversized images (larger than 200 KB) were detected.",
                );
            } else {
                let oversized_bytes: i64 = oversized.iter().map(|u| u.size.unwrap_or(0)).sum();
                status.add_summary_item_by_ranges(
                    "image-weight-oversized",
                    oversized.len() as f64,
                    &[(0.0, 0.0), (1.0, 10.0), (11.0, 50.0), (51.0, 100_000.0)],
                    &[
                        "0 oversized images",
                        "{} oversized image(s) (> 200 KB), {} total",
                        "{} oversized images (> 200 KB), {} total",
                        "{} oversized images (> 200 KB), {} total",
                    ],
                );
                let _ = oversized_bytes;
            }
        }

        self.base.measure_exec_time("ImageWeightAnalyzer", "analyze", s);
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        124
    }

    fn get_name(&self) -> &str {
        "ImageWeightAnalyzer"
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
    fn format_detection() {
        assert_eq!(ImageWeightAnalyzer::image_format(Some("image/webp"), "x"), "WebP");
        assert_eq!(ImageWeightAnalyzer::image_format(Some("image/png"), "x"), "PNG");
        assert_eq!(
            ImageWeightAnalyzer::image_format(None, "https://ex.com/logo.ico"),
            "ICO"
        );
        assert_eq!(
            ImageWeightAnalyzer::image_format(None, "https://ex.com/logo.png"),
            "Other"
        );
    }
}
