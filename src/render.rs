use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;
use regex::Regex;
use serde::Serialize;

use crate::xml_parser::{Article, LawDetail, LawMetadata};

const CHILD_SUFFIXES: [(&str, &str); 2] = [(" 시행규칙", "시행규칙"), (" 시행령", "시행령")];

fn type_to_filename(law_type: &str) -> &str {
    match law_type {
        "헌법" => "헌법",
        "법률" => "법률",
        "대통령령" => "대통령령",
        "총리령" => "총리령",
        "부령" => "부령",
        "대법원규칙" => "대법원규칙",
        "국회규칙" => "국회규칙",
        "헌법재판소규칙" => "헌법재판소규칙",
        "감사원규칙" => "감사원규칙",
        "선거관리위원회규칙" => "선거관리위원회규칙",
        _ => law_type,
    }
}

#[derive(Debug, Default)]
pub struct PathRegistry {
    assigned: HashMap<String, (String, String)>,
}

impl PathRegistry {
    pub fn get_law_path(&mut self, law_name: &str, law_type: &str) -> String {
        let (group, filename) = get_group_and_filename(law_name, law_type);
        let base = format!("kr/{group}/{filename}.md");
        if let Some(existing) = self.assigned.get(&base)
            && existing != &(law_name.to_owned(), law_type.to_owned())
        {
            let qualified = format!("kr/{group}/{filename}({law_type}).md");
            self.assigned.insert(
                qualified.clone(),
                (law_name.to_owned(), law_type.to_owned()),
            );
            return qualified;
        }

        self.assigned
            .insert(base.clone(), (law_name.to_owned(), law_type.to_owned()));
        base
    }
}

pub fn normalize_law_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '\u{00B7}' | '\u{30FB}' | '\u{FF65}' => '\u{318D}',
            _ => ch,
        })
        .collect()
}

pub fn format_date(date: &str) -> String {
    if date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..8])
    } else {
        date.to_owned()
    }
}

pub fn build_commit_message(metadata: &LawMetadata, mst: &str) -> String {
    let normalized = normalize_law_name(&metadata.law_name);
    let compact = normalized.replace(' ', "");
    let departments = if metadata.department_name.is_empty() {
        "미상".to_owned()
    } else {
        metadata.department_name.clone()
    };
    let prom_date = format_date(&metadata.promulgation_date);
    let prom_num = metadata.promulgation_number.clone();
    let prom_raw = metadata.promulgation_date.replace('-', "");
    let field = if metadata.field.is_empty() {
        "미분류".to_owned()
    } else {
        metadata.field.clone()
    };

    let mut title = format!("{}: {}", metadata.law_type, normalized);
    if !metadata.amendment.is_empty() {
        title.push_str(&format!(" ({})", metadata.amendment));
    }

    let url_law = format!("https://www.law.go.kr/법령/{compact}");
    let url_diff = format!("https://www.law.go.kr/법령/신구법비교/{compact}");

    let mut lines = vec![title, String::new()];
    lines.push(format!("법령 전문: {url_law}"));
    if !prom_num.is_empty() {
        lines.push(format!(
            "제개정문: https://www.law.go.kr/법령/제개정문/{compact}/({prom_num},{prom_raw})"
        ));
    }
    lines.push(format!("신구법비교: {url_diff}"));
    lines.push(String::new());
    lines.push(format!("공포일자: {prom_date}"));
    lines.push(format!("공포번호: {prom_num}"));
    lines.push(format!("소관부처: {departments}"));
    lines.push(format!("법령분야: {field}"));
    lines.push(format!("법령MST: {mst}"));
    lines.join("\n")
}

pub fn law_to_markdown(detail: &LawDetail) -> Result<Vec<u8>> {
    let frontmatter = build_frontmatter(&detail.metadata);
    let mut yaml = serde_yaml::to_string(&frontmatter)?;
    if let Some(stripped) = yaml.strip_prefix("---\n") {
        yaml = stripped.to_owned();
    }

    let normalized_name = normalize_law_name(&detail.metadata.law_name);
    let mut body_parts = vec![format!("# {normalized_name}"), String::new()];

    let articles = articles_to_markdown(&detail.articles);
    if !articles.is_empty() {
        body_parts.push(articles);
    }

    if !detail.addenda.is_empty() {
        body_parts.push(String::from("## 부칙"));
        body_parts.push(String::new());
        for addendum in &detail.addenda {
            let content = addendum.content.trim();
            if !content.is_empty() {
                body_parts.push(dedent_content(content));
                body_parts.push(String::new());
            }
        }
    }

    let body = body_parts.join("\n");
    Ok(format!("---\n{yaml}---\n\n{body}\n").into_bytes())
}

fn get_group_and_filename(law_name: &str, law_type: &str) -> (String, String) {
    let normalized = normalize_law_name(law_name);
    for (suffix, filename) in CHILD_SUFFIXES {
        if let Some(group) = normalized.strip_suffix(suffix) {
            return (group.replace(' ', ""), filename.to_owned());
        }
    }

    (
        normalized.replace(' ', ""),
        type_to_filename(law_type).to_owned(),
    )
}

fn build_frontmatter(metadata: &LawMetadata) -> Frontmatter {
    let raw_name = metadata.law_name.clone();
    let normalized = normalize_law_name(&raw_name);

    Frontmatter {
        title: normalized.clone(),
        mst: scalar_from_digits(&metadata.mst),
        law_id: metadata.law_id.clone(),
        law_type: metadata.law_type.clone(),
        law_type_code: metadata.law_type_code.clone(),
        departments: parse_departments(&metadata.department_name),
        promulgation_date: format_date(&metadata.promulgation_date),
        promulgation_number: metadata.promulgation_number.clone(),
        enforcement_date: format_date(&metadata.enforcement_date),
        field: metadata.field.clone(),
        status: String::from("시행"),
        source: format!("https://www.law.go.kr/법령/{}", normalized.replace(' ', "")),
        original_title: (normalized != raw_name).then_some(raw_name),
    }
}

fn articles_to_markdown(articles: &[Article]) -> String {
    let mut lines = Vec::new();

    for article in articles {
        let number = &article.number;
        let title = &article.title;
        let content = normalize_law_name(article.content.trim());

        if title.is_empty()
            && let Some(captures) = structure_re().captures(&content)
        {
            let level = match captures.get(1).map(|m| m.as_str()) {
                Some("편") => "#",
                Some("장") => "##",
                Some("절") => "###",
                Some("관") => "####",
                _ => "",
            };
            if !level.is_empty() {
                lines.push(format!("{level} {content}"));
                lines.push(String::new());
                continue;
            }
        }

        let mut heading = format!("##### 제{number}조");
        if !title.is_empty() {
            heading.push_str(&format!(" ({title})"));
        }
        lines.push(heading);
        lines.push(String::new());

        if !content.is_empty() {
            let cleaned = article_prefix_re().replace(&content, "").to_string();
            if !cleaned.is_empty() {
                lines.push(cleaned);
                lines.push(String::new());
            }
        }

        for paragraph in &article.paragraphs {
            let content = normalize_law_name(&paragraph.content);
            if !content.is_empty() {
                let stripped = circled_prefix_re().replace(content.trim(), "").to_string();
                let prefix = if paragraph.number.is_empty() {
                    String::new()
                } else {
                    format!("**{}**", paragraph.number)
                };
                lines.push(format!("{prefix} {stripped}"));
                lines.push(String::new());
            }

            for subparagraph in &paragraph.subparagraphs {
                let content = normalize_law_name(&subparagraph.content);
                if !content.is_empty() {
                    let stripped = ho_prefix_re().replace(content.trim(), "").to_string();
                    let stripped = normalize_ws(&stripped);
                    let number = subparagraph.number.trim().trim_end_matches('.');
                    if number.is_empty() {
                        lines.push(format!("  {stripped}"));
                    } else {
                        lines.push(format!("  {number}\\. {stripped}"));
                    }
                }

                for item in &subparagraph.items {
                    let content = normalize_law_name(&item.content);
                    if !content.is_empty() {
                        let stripped = mok_prefix_re().replace(content.trim(), "").to_string();
                        let stripped = normalize_ws(&stripped);
                        let number = item.number.trim().trim_end_matches('.');
                        if number.is_empty() {
                            lines.push(format!("    {stripped}"));
                        } else {
                            lines.push(format!("    {number}\\. {stripped}"));
                        }
                    }
                }
            }

            if !paragraph.subparagraphs.is_empty() {
                lines.push(String::new());
            }
        }
    }

    lines.join("\n")
}

fn dedent_content(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let min_indent = lines
        .iter()
        .filter_map(|line| {
            let stripped = line.trim_start();
            if stripped.is_empty() {
                None
            } else {
                let indent = line.len() - stripped.len();
                (indent > 0).then_some(indent)
            }
        })
        .min();

    let Some(min_indent) = min_indent else {
        return text.to_owned();
    };

    lines
        .into_iter()
        .map(|line| {
            let stripped = line.trim_start();
            if stripped.is_empty() {
                String::new()
            } else {
                let indent = line.len() - stripped.len();
                let new_indent = indent.saturating_sub(min_indent);
                format!("{}{}", " ".repeat(new_indent), stripped)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_ws(text: &str) -> String {
    whitespace_re().replace_all(text, " ").trim().to_owned()
}

fn structure_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"^제\d+(?:의\d+)?(편|장|절|관)\s*").unwrap())
}

fn article_prefix_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"^제\d+조(?:의\d+)?\s*(?:\([^)]*\)\s*)?").unwrap())
}

fn circled_prefix_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"^[①②③④⑤⑥⑦⑧⑨⑩⑪⑫⑬⑭⑮⑯⑰⑱⑲⑳]\s*").unwrap())
}

fn ho_prefix_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"^\d+(?:의\d+)?\.\s*").unwrap())
}

fn mok_prefix_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"^[가-힣](?:의\d+)?\.\s*").unwrap())
}

fn whitespace_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"[ \t]+").unwrap())
}

fn parse_departments(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn scalar_from_digits(value: &str) -> ScalarValue {
    match value.parse::<u64>() {
        Ok(number) => ScalarValue::Number(number),
        Err(_) => ScalarValue::String(value.to_owned()),
    }
}

#[derive(Debug, Serialize)]
struct Frontmatter {
    #[serde(rename = "제목")]
    title: String,
    #[serde(rename = "법령MST")]
    mst: ScalarValue,
    #[serde(rename = "법령ID")]
    law_id: String,
    #[serde(rename = "법령구분")]
    law_type: String,
    #[serde(rename = "법령구분코드")]
    law_type_code: String,
    #[serde(rename = "소관부처")]
    departments: Vec<String>,
    #[serde(rename = "공포일자")]
    promulgation_date: String,
    #[serde(rename = "공포번호")]
    promulgation_number: String,
    #[serde(rename = "시행일자")]
    enforcement_date: String,
    #[serde(rename = "법령분야")]
    field: String,
    #[serde(rename = "상태")]
    status: String,
    #[serde(rename = "출처")]
    source: String,
    #[serde(rename = "원본제목", skip_serializing_if = "Option::is_none")]
    original_title: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ScalarValue {
    Number(u64),
    String(String),
}

#[cfg(test)]
mod tests {
    use crate::xml_parser::{Addendum, Paragraph, Subparagraph};

    use super::*;

    #[test]
    fn path_registry_matches_existing_collision_rule() {
        let mut registry = PathRegistry::default();
        assert_eq!(
            registry.get_law_path("테스트법 시행규칙", "부령"),
            "kr/테스트법/시행규칙.md"
        );
        assert_eq!(
            registry.get_law_path("테스트법 시행규칙", "총리령"),
            "kr/테스트법/시행규칙(총리령).md"
        );
    }

    #[test]
    fn markdown_renders_python_style_lists_and_addenda() {
        let detail = LawDetail {
            metadata: LawMetadata {
                law_name: String::from("테스트법"),
                law_id: String::from("000001"),
                law_type: String::from("법률"),
                promulgation_date: String::from("20240101"),
                promulgation_number: String::from("00001"),
                enforcement_date: String::from("20240101"),
                department_name: String::from("법무부"),
                ..LawMetadata::default()
            },
            articles: vec![Article {
                number: String::from("1"),
                title: String::from("정의"),
                content: String::from("제1조 (정의) 본문"),
                paragraphs: vec![Paragraph {
                    number: String::from("①"),
                    content: String::from("①정의"),
                    subparagraphs: vec![Subparagraph {
                        number: String::from("1."),
                        content: String::from("1.  첫 호"),
                        items: vec![crate::xml_parser::Item {
                            number: String::from("가."),
                            content: String::from("가.  첫 목"),
                        }],
                    }],
                }],
            }],
            addenda: vec![Addendum {
                content: String::from("    부칙 본문"),
            }],
        };

        let markdown = String::from_utf8(law_to_markdown(&detail).unwrap()).unwrap();
        assert!(markdown.contains("##### 제1조 (정의)"));
        assert!(markdown.contains("  1\\. 첫 호"));
        assert!(markdown.contains("    가\\. 첫 목"));
        assert!(markdown.contains("## 부칙"));
    }
}
