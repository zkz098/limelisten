//! DOCX（OOXML）纯文本提取：读取 `word/document.xml`，按段落输出文本。
//!
//! 只支持 `.docx`；旧版 `.doc` 是二进制复合文档，需要外部转换器。

use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::Reader;
use std::io::{Cursor, Read};

const DOCUMENT_XML: &str = "word/document.xml";
const MAX_DOCUMENT_XML_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DocxError {
    #[error("不是有效的 .docx 文件（可能已加密或为旧版 .doc）：{0}")]
    Zip(String),
    #[error("文档缺少 word/document.xml")]
    MissingDocument,
    #[error("文档内容过大（document.xml 超过 32 MB）")]
    TooLarge,
    #[error("读取文档失败：{0}")]
    Io(String),
    #[error("解析文档 XML 失败：{0}")]
    Xml(String),
    #[error("未能从文档中提取到文字（内容可能是图片或扫描件）")]
    Empty,
}

/// 从 `.docx` 字节中提取纯文本，每个 Word 段落输出一行。
pub fn extract_docx_text(bytes: &[u8]) -> Result<String, DocxError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| DocxError::Zip(e.to_string()))?;
    let mut document = match archive.by_name(DOCUMENT_XML) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Err(DocxError::MissingDocument),
        Err(e) => return Err(DocxError::Zip(e.to_string())),
    };
    if document.size() > MAX_DOCUMENT_XML_BYTES {
        return Err(DocxError::TooLarge);
    }

    let mut xml = String::new();
    document
        .read_to_string(&mut xml)
        .map_err(|e| DocxError::Io(e.to_string()))?;
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(&xml);

    let text = parse_document_xml(xml)?;
    if text.is_empty() {
        return Err(DocxError::Empty);
    }
    Ok(text)
}

fn parse_document_xml(xml: &str) -> Result<String, DocxError> {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut paragraph = String::new();
    let mut in_text = false;
    let mut fallback_depth = 0usize;

    loop {
        let event = reader
            .read_event()
            .map_err(|e| DocxError::Xml(e.to_string()))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                let name = local_name(e.name());
                if name == "Fallback" {
                    // 文本框常同时存在 Choice/Fallback，跳过 Fallback 避免文字重复
                    fallback_depth += 1;
                    continue;
                }
                if fallback_depth > 0 {
                    continue;
                }
                match name {
                    "p" => paragraph.clear(),
                    "t" => in_text = true,
                    "tab" => push_space(&mut paragraph),
                    "br" | "cr" => push_newline(&mut paragraph),
                    "noBreakHyphen" => paragraph.push('-'),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if fallback_depth > 0 {
                    continue;
                }
                match local_name(e.name()) {
                    "tab" => push_space(&mut paragraph),
                    "br" | "cr" => push_newline(&mut paragraph),
                    "noBreakHyphen" => paragraph.push('-'),
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = local_name(e.name());
                if name == "Fallback" {
                    fallback_depth = fallback_depth.saturating_sub(1);
                    continue;
                }
                if fallback_depth > 0 {
                    continue;
                }
                match name {
                    "p" => {
                        in_text = false;
                        flush_paragraph(&mut out, &mut paragraph);
                    }
                    "t" => in_text = false,
                    _ => {}
                }
            }
            Event::Text(e) if in_text && fallback_depth == 0 => {
                paragraph.push_str(&e.into_inner());
            }
            Event::GeneralRef(e) if in_text && fallback_depth == 0 => {
                let entity = format!("&{};", e.into_inner());
                if let Ok(decoded) = quick_xml::escape::unescape(&entity) {
                    paragraph.push_str(&decoded);
                }
            }
            _ => {}
        }
    }

    Ok(out)
}

fn local_name<'a>(name: QName<'a>) -> &'a str {
    name.local_name().into_inner()
}

fn push_space(s: &mut String) {
    if !s.ends_with(' ') && !s.ends_with('\n') {
        s.push(' ');
    }
}

fn push_newline(s: &mut String) {
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
}

fn flush_paragraph(out: &mut String, paragraph: &mut String) {
    let line = paragraph.trim();
    if !line.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    paragraph.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExerciseParser;
    use std::io::Write;

    fn build_docx(document_xml: &str) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file(DOCUMENT_XML, options).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn wrap_document(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
  <w:body>{body}</w:body>
</w:document>"#
        )
    }

    #[test]
    fn extracts_paragraphs_runs_tabs_breaks_and_tables() {
        let xml = wrap_document(
            r#"
<w:p><w:r><w:t>Text 1</w:t></w:r></w:p>
<w:p>
  <w:r><w:t>1. What is the woman doing?</w:t></w:r>
</w:p>
<w:p>
  <w:r><w:t>A. Driving a car.</w:t><w:tab/><w:t>B. Walking in the park.</w:t></w:r>
</w:p>
<w:p><w:r><w:t>答案: </w:t></w:r><w:r><w:t>C</w:t></w:r></w:p>
<w:p><w:r><w:t>解析: 女士在车站。</w:t></w:r></w:p>
<w:p>
  <w:r><w:t>first</w:t><w:br/><w:t>second</w:t></w:r>
</w:p>
<w:tbl>
  <w:tr><w:tc><w:p><w:r><w:t>2. Where are the speakers?</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>"#,
        );

        let text = extract_docx_text(&build_docx(&xml)).unwrap();
        assert_eq!(
            text,
            "Text 1\n1. What is the woman doing?\nA. Driving a car. B. Walking in the park.\n\
             答案: C\n解析: 女士在车站。\nfirst\nsecond\n2. Where are the speakers?"
        );

        let parsed = ExerciseParser::parse(&text, 1);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].questions.len(), 2);
        assert_eq!(parsed[0].questions[0].options.len(), 2);
        assert_eq!(parsed[0].questions[0].answer.as_deref(), Some("C"));
    }

    #[test]
    fn decodes_entities_and_ignores_deleted_text() {
        let xml = wrap_document(
            r#"
<w:p>
  <w:del><w:r><w:delText>deleted</w:delText></w:r></w:del>
  <w:r><w:t>Q&amp;A &lt;ok&gt;</w:t></w:r>
</w:p>"#,
        );

        let text = extract_docx_text(&build_docx(&xml)).unwrap();
        assert_eq!(text, "Q&A <ok>");
        assert!(!text.contains("deleted"));
    }

    #[test]
    fn skips_fallback_copies_of_textboxes() {
        let xml = wrap_document(
            r#"
<w:p><w:r><w:t>choice</w:t></w:r></w:p>
<mc:AlternateContent>
  <mc:Choice><w:p><w:r><w:t>boxed</w:t></w:r></w:p></mc:Choice>
  <mc:Fallback><w:p><w:r><w:t>boxed</w:t></w:r></w:p></mc:Fallback>
</mc:AlternateContent>"#,
        );

        let text = extract_docx_text(&build_docx(&xml)).unwrap();
        assert_eq!(text, "choice\nboxed");
    }

    #[test]
    fn strips_xml_bom_and_rejects_invalid_or_empty_documents() {
        let with_bom = format!(
            "\u{feff}{}",
            wrap_document("<w:p><w:r><w:t>ok</w:t></w:r></w:p>")
        );
        assert_eq!(extract_docx_text(&build_docx(&with_bom)).unwrap(), "ok");

        assert!(matches!(
            extract_docx_text(b"not a zip"),
            Err(DocxError::Zip(_))
        ));

        let empty = wrap_document("<w:p><w:r><w:t> </w:t></w:r></w:p>");
        assert!(matches!(
            extract_docx_text(&build_docx(&empty)),
            Err(DocxError::Empty)
        ));
    }
}
