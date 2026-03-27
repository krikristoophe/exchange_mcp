use anyhow::{Context, Result};
use base64::Engine;

use super::client::{AttachmentInfo, EmailDetail, EmailSummary};

pub fn parse_email_summary(msg: &imap::types::Fetch) -> Option<EmailSummary> {
    let uid = msg.uid?;
    let envelope = msg.envelope()?;

    let subject = envelope
        .subject
        .as_ref()
        .map(|s| decode_imap_utf8(s))
        .unwrap_or_default();

    let from = envelope
        .from
        .as_ref()
        .and_then(|addrs| addrs.first())
        .map(|addr| format_address(addr))
        .unwrap_or_default();

    let date = envelope
        .date
        .as_ref()
        .map(|d| String::from_utf8_lossy(d).to_string())
        .unwrap_or_default();

    let flags: Vec<String> = msg.flags().iter().map(|f| format!("{f:?}")).collect();

    Some(EmailSummary {
        uid,
        subject,
        from,
        date,
        flags,
        size: msg.size,
        snippet: None,
    })
}

/// Extract a text snippet from a partially fetched email body.
/// Used with BODY.PEEK[TEXT]<0.N> fetches.
pub fn extract_snippet(msg: &imap::types::Fetch, max_chars: usize) -> Option<String> {
    let body = msg.body()?;
    if body.is_empty() {
        return None;
    }

    // Try to parse as MIME and extract plain text
    let text = match mailparse::parse_mail(body) {
        Ok(parsed) => {
            let mut text_body = String::new();
            extract_text_only(&parsed, &mut text_body);
            if text_body.is_empty() {
                // Fallback: if it's HTML, convert
                let mut html_body = None;
                extract_html_only(&parsed, &mut html_body);
                if let Some(html) = html_body {
                    super::html_to_text(&html)
                } else {
                    parsed.get_body().unwrap_or_default()
                }
            } else {
                text_body
            }
        }
        Err(_) => {
            // Raw text fallback
            String::from_utf8_lossy(body).to_string()
        }
    };

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Truncate to max_chars on a word boundary
    let snippet = if trimmed.len() <= max_chars {
        trimmed.to_string()
    } else {
        let truncated = &trimmed[..max_chars];
        match truncated.rfind(' ') {
            Some(pos) if pos > max_chars / 2 => format!("{}...", &truncated[..pos]),
            _ => format!("{truncated}..."),
        }
    };

    // Collapse whitespace
    let snippet = snippet
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    Some(snippet)
}

fn extract_text_only(mail: &mailparse::ParsedMail, text_body: &mut String) {
    if mail.subparts.is_empty() {
        let content_type = mail.ctype.mimetype.to_lowercase();
        if content_type.contains("text/plain") {
            if let Ok(body) = mail.get_body() {
                if !text_body.is_empty() {
                    text_body.push('\n');
                }
                text_body.push_str(&body);
            }
        }
    } else {
        for part in &mail.subparts {
            extract_text_only(part, text_body);
        }
    }
}

fn extract_html_only(mail: &mailparse::ParsedMail, html_body: &mut Option<String>) {
    if mail.subparts.is_empty() {
        let content_type = mail.ctype.mimetype.to_lowercase();
        if content_type.contains("text/html") && html_body.is_none() {
            if let Ok(body) = mail.get_body() {
                *html_body = Some(body);
            }
        }
    } else {
        for part in &mail.subparts {
            extract_html_only(part, html_body);
        }
    }
}

pub fn parse_email_detail(msg: &imap::types::Fetch) -> Result<EmailDetail> {
    let uid = msg.uid.context("No UID")?;
    let envelope = msg.envelope().context("No envelope")?;

    let subject = envelope
        .subject
        .as_ref()
        .map(|s| decode_imap_utf8(s))
        .unwrap_or_default();

    let from = format_addresses(envelope.from.as_deref());
    let to = format_addresses(envelope.to.as_deref());
    let cc = format_addresses(envelope.cc.as_deref());

    let date = envelope
        .date
        .as_ref()
        .map(|d| String::from_utf8_lossy(d).to_string())
        .unwrap_or_default();

    let flags: Vec<String> = msg.flags().iter().map(|f| format!("{f:?}")).collect();

    let (body_text, body_html, attachments) = if let Some(body) = msg.body() {
        let (text, html) = parse_body(body);
        let attachments = extract_attachments(body);
        (text, html, attachments)
    } else {
        ("(no body)".to_string(), None, vec![])
    };

    Ok(EmailDetail {
        uid,
        subject,
        from,
        to,
        cc,
        date,
        flags,
        body_text,
        body_html,
        attachments,
    })
}

pub(crate) fn format_address(addr: &imap_proto::types::Address<'_>) -> String {
    let name = addr
        .name
        .as_ref()
        .map(|n| decode_imap_utf8(n))
        .unwrap_or_default();
    let mailbox = addr
        .mailbox
        .as_ref()
        .map(|m| String::from_utf8_lossy(m).to_string())
        .unwrap_or_default();
    let host = addr
        .host
        .as_ref()
        .map(|h| String::from_utf8_lossy(h).to_string())
        .unwrap_or_default();
    if name.is_empty() {
        format!("{mailbox}@{host}")
    } else {
        format!("{name} <{mailbox}@{host}>")
    }
}

fn format_addresses(addrs: Option<&[imap_proto::types::Address<'_>]>) -> String {
    addrs
        .map(|addrs: &[imap_proto::types::Address<'_>]| {
            addrs
                .iter()
                .map(|addr| format_address(addr))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

pub(crate) fn decode_imap_utf8(data: &[u8]) -> String {
    let s = String::from_utf8_lossy(data).to_string();
    if s.contains("=?") {
        decode_rfc2047(&s)
    } else {
        s
    }
}

/// Public wrapper for decode_rfc2047, used by contact extraction.
pub fn decode_rfc2047_public(s: &str) -> String {
    decode_rfc2047(s)
}

fn decode_rfc2047(s: &str) -> String {
    let mut result = String::new();
    let mut remaining = s;

    while let Some(start) = remaining.find("=?") {
        result.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];

        if let Some(end) = after_start.find("?=") {
            let encoded_word = &after_start[..end];
            let parts: Vec<&str> = encoded_word.splitn(3, '?').collect();
            if parts.len() == 3 {
                let charset = parts[0];
                let encoding = parts[1].to_uppercase();
                let text = parts[2];

                let raw_bytes = match encoding.as_str() {
                    "B" => base64::engine::general_purpose::STANDARD
                        .decode(text)
                        .ok(),
                    "Q" => {
                        let text = text.replace('_', " ");
                        quoted_printable_decode_bytes(&text)
                    }
                    _ => None,
                };

                if let Some(bytes) = raw_bytes {
                    let decoded = decode_charset(charset, &bytes);
                    result.push_str(&decoded);
                } else {
                    result.push_str(&remaining[start..start + 2 + end + 2]);
                }
            }
            remaining = &after_start[end + 2..];
            // Skip whitespace between consecutive encoded words (RFC 2047 section 6.2)
            if remaining.starts_with(' ') || remaining.starts_with('\t') {
                if let Some(next) = remaining.find("=?") {
                    let between = &remaining[..next];
                    if between.trim().is_empty() {
                        remaining = &remaining[next..];
                    }
                }
            }
        } else {
            result.push_str(&remaining[start..]);
            remaining = "";
        }
    }
    result.push_str(remaining);
    result
}

/// Decode raw bytes from a given charset to UTF-8, using encoding_rs.
fn decode_charset(charset: &str, bytes: &[u8]) -> String {
    let charset_lower = charset.to_lowercase();

    // UTF-8 is already valid
    if charset_lower == "utf-8" || charset_lower == "us-ascii" || charset_lower == "ascii" {
        return String::from_utf8_lossy(bytes).to_string();
    }

    // Use encoding_rs for charset conversion
    if let Some(encoding) = encoding_rs::Encoding::for_label(charset.as_bytes()) {
        let (decoded, _, _) = encoding.decode(bytes);
        decoded.into_owned()
    } else {
        // Fallback: lossy UTF-8
        tracing::warn!("Unknown charset '{charset}', using lossy UTF-8");
        String::from_utf8_lossy(bytes).to_string()
    }
}

fn quoted_printable_decode_bytes(s: &str) -> Option<Vec<u8>> {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
            {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    Some(result)
}

fn parse_body(body: &[u8]) -> (String, Option<String>) {
    match mailparse::parse_mail(body) {
        Ok(parsed) => {
            let mut text_body = String::new();
            let mut html_body = None;

            extract_parts(&parsed, &mut text_body, &mut html_body);

            if text_body.is_empty() && html_body.is_none() {
                text_body = parsed
                    .get_body()
                    .unwrap_or_else(|_| String::from_utf8_lossy(body).to_string());
            }

            (text_body, html_body)
        }
        Err(_) => (String::from_utf8_lossy(body).to_string(), None),
    }
}

fn extract_parts(
    mail: &mailparse::ParsedMail,
    text_body: &mut String,
    html_body: &mut Option<String>,
) {
    if mail.subparts.is_empty() {
        let content_type = mail.ctype.mimetype.to_lowercase();
        // Skip attachments
        let disposition = mail
            .headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case("content-disposition"))
            .map(|h| h.get_value())
            .unwrap_or_default();
        if disposition.starts_with("attachment") {
            return;
        }
        if let Ok(body) = mail.get_body() {
            if content_type.contains("text/plain") {
                if !text_body.is_empty() {
                    text_body.push('\n');
                }
                text_body.push_str(&body);
            } else if content_type.contains("text/html") {
                *html_body = Some(body);
            }
        }
    } else {
        for part in &mail.subparts {
            extract_parts(part, text_body, html_body);
        }
    }
}

fn extract_attachments(body: &[u8]) -> Vec<AttachmentInfo> {
    let mut attachments = Vec::new();
    if let Ok(parsed) = mailparse::parse_mail(body) {
        collect_attachments(&parsed, &mut attachments);
    }
    attachments
}

fn collect_attachments(mail: &mailparse::ParsedMail, attachments: &mut Vec<AttachmentInfo>) {
    if mail.subparts.is_empty() {
        let content_type = mail.ctype.mimetype.to_lowercase();
        let disposition = mail
            .headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case("content-disposition"))
            .map(|h| h.get_value())
            .unwrap_or_default();

        let is_attachment = disposition.starts_with("attachment")
            || (!content_type.contains("text/plain")
                && !content_type.contains("text/html")
                && !content_type.starts_with("multipart/"));

        if is_attachment {
            let filename = mail
                .ctype
                .params
                .get("name")
                .cloned()
                .or_else(|| {
                    // Try content-disposition filename param
                    extract_disposition_filename(&disposition)
                })
                .unwrap_or_else(|| "unnamed".to_string());

            let size = mail.get_body_raw().ok().map(|b| b.len());

            attachments.push(AttachmentInfo {
                filename,
                content_type: content_type.to_string(),
                size,
            });
        }
    } else {
        for part in &mail.subparts {
            collect_attachments(part, attachments);
        }
    }
}

/// Find the first MIME part whose filename matches (case-insensitive).
/// Filenames are decoded from RFC 2047 encoding before comparison.
#[allow(dead_code)]
pub fn find_attachment_part<'a>(
    mail: &'a mailparse::ParsedMail<'a>,
    target_filename: &str,
) -> Option<&'a mailparse::ParsedMail<'a>> {
    let target = target_filename.to_lowercase();
    find_part_recursive(mail, &target)
}

#[allow(dead_code)]
fn find_part_recursive<'a>(
    mail: &'a mailparse::ParsedMail<'a>,
    target: &str,
) -> Option<&'a mailparse::ParsedMail<'a>> {
    // Note: we only inspect leaf parts (no subparts). This matches the behavior of
    // collect_attachments. A message/rfc822 attachment's outer filename is not matched
    // because the parser populates subparts for it.
    if mail.subparts.is_empty() {
        let disposition = mail
            .headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case("content-disposition"))
            .map(|h| h.get_value())
            .unwrap_or_default();

        let raw_name = mail
            .ctype
            .params
            .get("name")
            .cloned()
            .or_else(|| extract_disposition_filename(&disposition));

        if let Some(raw) = raw_name {
            let decoded = decode_rfc2047_public(&raw);
            if decoded.to_lowercase() == target {
                return Some(mail);
            }
        }
        None
    } else {
        for part in &mail.subparts {
            if let Some(found) = find_part_recursive(part, target) {
                return Some(found);
            }
        }
        None
    }
}

fn extract_disposition_filename(disposition: &str) -> Option<String> {
    // Parse "attachment; filename=\"file.pdf\""
    for part in disposition.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename=") {
            let name = rest.trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Strip quoted replies from email text body.
/// Removes content after common reply markers.
pub fn strip_quoted_replies(text: &str) -> String {
    let mut result = String::new();
    let mut in_quote = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Detect start of quoted content
        if !in_quote {
            // Common reply separators
            if trimmed == "---"
                || trimmed == "___"
                || trimmed.starts_with("-----Original Message-----")
                || trimmed.starts_with("________________________________")
                || trimmed.starts_with("On ") && trimmed.ends_with("wrote:")
                || trimmed.starts_with("Le ") && trimmed.ends_with("a ecrit :")
                || trimmed.starts_with("Le ") && trimmed.ends_with("a ecrit:")
                || trimmed.starts_with("De :") || trimmed.starts_with("De:")
                || trimmed.starts_with("From:") && trimmed.contains("Sent:")
                || trimmed.starts_with("> -----Original")
            {
                in_quote = true;
                continue;
            }

            // Lines starting with > are quoted
            if trimmed.starts_with('>') {
                // Skip individual quoted lines but don't stop processing
                continue;
            }

            result.push_str(line);
            result.push('\n');
        }
        // Once in_quote is true, we skip all remaining lines
    }

    result.trim_end().to_string()
}

/// Simple HTML to text converter -- strips tags and decodes common entities
pub fn html_to_text(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_space = false;

    let html_lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let lower_bytes = html_lower.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if in_script {
            if i + 9 <= len && &lower_bytes[i..i + 9] == b"</script>" {
                in_script = false;
                i += 9;
            } else {
                i += 1;
            }
            continue;
        }
        if in_style {
            if i + 8 <= len && &lower_bytes[i..i + 8] == b"</style>" {
                in_style = false;
                i += 8;
            } else {
                i += 1;
            }
            continue;
        }

        if bytes[i] == b'<' {
            // Check for block elements that should add newlines
            let rest = &html_lower[i..];
            if rest.starts_with("<br") || rest.starts_with("<p") || rest.starts_with("<div")
                || rest.starts_with("<tr") || rest.starts_with("<li")
                || rest.starts_with("</p") || rest.starts_with("</div")
                || rest.starts_with("</tr") || rest.starts_with("<h")
                || rest.starts_with("</h")
            {
                if !result.ends_with('\n') {
                    result.push('\n');
                }
                last_was_space = true;
            }
            if rest.starts_with("<script") {
                in_script = true;
            } else if rest.starts_with("<style") {
                in_style = true;
            }
            in_tag = true;
            i += 1;
        } else if bytes[i] == b'>' {
            in_tag = false;
            i += 1;
        } else if in_tag {
            i += 1;
        } else if bytes[i] == b'&' {
            // Decode HTML entities
            let rest = &html[i..];
            if rest.starts_with("&amp;") {
                result.push('&');
                i += 5;
            } else if rest.starts_with("&lt;") {
                result.push('<');
                i += 4;
            } else if rest.starts_with("&gt;") {
                result.push('>');
                i += 4;
            } else if rest.starts_with("&quot;") {
                result.push('"');
                i += 6;
            } else if rest.starts_with("&apos;") {
                result.push('\'');
                i += 6;
            } else if rest.starts_with("&nbsp;") {
                result.push(' ');
                i += 6;
            } else if rest.starts_with("&#") {
                if let Some(semi) = rest[2..].find(';') {
                    let num_str = &rest[2..2 + semi];
                    let code = if let Some(hex) = num_str.strip_prefix('x') {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num_str.parse::<u32>().ok()
                    };
                    if let Some(ch) = code.and_then(char::from_u32) {
                        result.push(ch);
                    }
                    i += 2 + semi + 1;
                } else {
                    result.push('&');
                    i += 1;
                }
            } else {
                result.push('&');
                i += 1;
            }
            last_was_space = false;
        } else {
            let ch = bytes[i] as char;
            if ch.is_whitespace() {
                if !last_was_space {
                    result.push(if ch == '\n' { '\n' } else { ' ' });
                    last_was_space = true;
                }
            } else {
                result.push(ch);
                last_was_space = false;
            }
            i += 1;
        }
    }

    // Clean up excessive newlines
    let mut cleaned = String::new();
    let mut newline_count = 0;
    for ch in result.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                cleaned.push(ch);
            }
        } else {
            newline_count = 0;
            cleaned.push(ch);
        }
    }

    cleaned.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Email MIME minimal avec une piece jointe text/plain nommee "hello.txt"
    fn make_email_with_attachment(filename: &str, content: &str) -> Vec<u8> {
        format!(
            "From: test@example.com\r\n\
             To: dest@example.com\r\n\
             Subject: Test\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"bound\"\r\n\
             \r\n\
             --bound\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             Body text\r\n\
             --bound\r\n\
             Content-Type: application/octet-stream; name=\"{filename}\"\r\n\
             Content-Disposition: attachment; filename=\"{filename}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             {}\r\n\
             --bound--\r\n",
            base64::engine::general_purpose::STANDARD.encode(content.as_bytes())
        )
        .into_bytes()
    }

    #[test]
    fn test_find_attachment_part_found() {
        let raw = make_email_with_attachment("report.pdf", "PDF content");
        let parsed = mailparse::parse_mail(&raw).unwrap();
        let part = find_attachment_part(&parsed, "report.pdf");
        assert!(part.is_some());
        let part = part.unwrap();
        assert_eq!(part.ctype.mimetype, "application/octet-stream");
    }

    #[test]
    fn test_find_attachment_part_case_insensitive() {
        let raw = make_email_with_attachment("Report.PDF", "PDF content");
        let parsed = mailparse::parse_mail(&raw).unwrap();
        let part = find_attachment_part(&parsed, "report.pdf");
        assert!(part.is_some());
    }

    #[test]
    fn test_find_attachment_part_not_found() {
        let raw = make_email_with_attachment("other.pdf", "PDF content");
        let parsed = mailparse::parse_mail(&raw).unwrap();
        let part = find_attachment_part(&parsed, "missing.pdf");
        assert!(part.is_none());
    }

    #[test]
    fn test_find_attachment_part_rfc2047_filename() {
        // "report.pdf" encoded as RFC 2047 base64
        let encoded_filename = "=?utf-8?b?cmVwb3J0LnBkZg==?=";
        let raw = format!(
            "From: test@example.com\r\n\
             To: dest@example.com\r\n\
             Subject: Test\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"bound\"\r\n\
             \r\n\
             --bound\r\n\
             Content-Type: application/octet-stream; name=\"{encoded_filename}\"\r\n\
             Content-Disposition: attachment; filename=\"{encoded_filename}\"\r\n\
             \r\n\
             data\r\n\
             --bound--\r\n"
        ).into_bytes();
        let parsed = mailparse::parse_mail(&raw).unwrap();
        let part = find_attachment_part(&parsed, "report.pdf");
        assert!(part.is_some(), "Should find attachment with RFC 2047 encoded filename");
    }
}
