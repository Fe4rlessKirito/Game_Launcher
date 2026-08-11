use crate::domain::ProvisioningError;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::str::FromStr;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailIngestHeaders {
    pub timestamp: i64,
    pub nonce: String,
    pub signature: String,
    pub envelope_from: Option<String>,
    pub envelope_to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailIngestVerification {
    pub timestamp: i64,
    pub nonce: String,
    pub body_sha256: String,
    pub envelope_from: Option<String>,
    pub envelope_to: String,
}

pub fn canonical_email_payload(
    timestamp: i64,
    nonce: &str,
    body_sha256: &str,
    envelope_from: Option<&str>,
    envelope_to: &str,
) -> String {
    format!(
        "{timestamp}\n{nonce}\n{body_sha256}\n{}\n{envelope_to}",
        envelope_from.unwrap_or_default()
    )
}

pub fn compute_email_hmac(
    secret: &[u8],
    timestamp: i64,
    nonce: &str,
    envelope_from: Option<&str>,
    envelope_to: &str,
    body: &[u8],
) -> Result<String, ProvisioningError> {
    if secret.is_empty() {
        return Err(ProvisioningError::Configuration(
            "email HMAC secret is empty".to_owned(),
        ));
    }
    let body_sha256 = sha256_hex(body);
    let canonical =
        canonical_email_payload(timestamp, nonce, &body_sha256, envelope_from, envelope_to);
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| ProvisioningError::Configuration("invalid HMAC secret".to_owned()))?;
    mac.update(canonical.as_bytes());
    Ok(STANDARD.encode(mac.finalize().into_bytes()))
}

pub fn verify_email_ingest(
    headers: &EmailIngestHeaders,
    secret: &[u8],
    body: &[u8],
    now: DateTime<Utc>,
    allowed_clock_skew: Duration,
) -> Result<EmailIngestVerification, ProvisioningError> {
    if headers.nonce.trim().is_empty() || headers.envelope_to.trim().is_empty() {
        return Err(ProvisioningError::Security(
            "missing mail nonce or recipient".to_owned(),
        ));
    }
    let timestamp = DateTime::<Utc>::from_timestamp(headers.timestamp, 0)
        .ok_or_else(|| ProvisioningError::Security("invalid mail timestamp".to_owned()))?;
    let age = now.signed_duration_since(timestamp).num_seconds().abs();
    if age > allowed_clock_skew.num_seconds().max(0) {
        return Err(ProvisioningError::Security(
            "mail signature timestamp is outside the allowed skew".to_owned(),
        ));
    }
    let body_sha256 = sha256_hex(body);
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| ProvisioningError::Configuration("invalid HMAC secret".to_owned()))?;
    let canonical = canonical_email_payload(
        headers.timestamp,
        &headers.nonce,
        &body_sha256,
        headers.envelope_from.as_deref(),
        &headers.envelope_to,
    );
    mac.update(canonical.as_bytes());
    let supplied = STANDARD.decode(headers.signature.as_bytes()).map_err(|_| {
        ProvisioningError::Security("mail signature is not valid base64".to_owned())
    })?;
    mac.verify_slice(&supplied)
        .map_err(|_| ProvisioningError::Security("mail signature mismatch".to_owned()))?;
    Ok(EmailIngestVerification {
        timestamp: headers.timestamp,
        nonce: headers.nonce.clone(),
        body_sha256,
        envelope_from: headers.envelope_from.clone(),
        envelope_to: headers.envelope_to.clone(),
    })
}

pub fn sha256_hex(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedMail {
    pub message_id: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub text_plain: Option<String>,
    pub text_html: Option<String>,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProvisioningEmailEvent {
    VerificationCodeReceived { token: String },
    VerificationLinkReceived { reference: String },
    ProviderReady,
}

pub trait ProvisioningEmailParser: Send + Sync {
    fn provider_type(&self) -> &str;
    fn matches(&self, mail: &ParsedMail) -> bool;
    fn parse(&self, mail: &ParsedMail) -> Result<ProvisioningEmailEvent, ProvisioningError>;
}

#[derive(Default)]
pub struct ProvisioningEmailParserRegistry {
    parsers: BTreeMap<String, Box<dyn ProvisioningEmailParser>>,
}

impl ProvisioningEmailParserRegistry {
    pub fn register(&mut self, parser: Box<dyn ProvisioningEmailParser>) {
        self.parsers
            .insert(parser.provider_type().to_owned(), parser);
    }

    pub fn parse(
        &self,
        provider_type: &str,
        mail: &ParsedMail,
    ) -> Result<ProvisioningEmailEvent, ProvisioningError> {
        let parser = self.parsers.get(provider_type).ok_or_else(|| {
            ProvisioningError::Mail(format!("no email parser registered for {provider_type}"))
        })?;
        if !parser.matches(mail) {
            return Err(ProvisioningError::Mail(
                "email does not match the provider parser".to_owned(),
            ));
        }
        parser.parse(mail)
    }
}

pub struct FakeProvisioningEmailParser;

impl ProvisioningEmailParser for FakeProvisioningEmailParser {
    fn provider_type(&self) -> &str {
        "fake"
    }

    fn matches(&self, mail: &ParsedMail) -> bool {
        mail.subject
            .as_deref()
            .is_some_and(|subject| subject.to_ascii_lowercase().contains("fake verification"))
            || mail
                .text_plain
                .as_deref()
                .is_some_and(|body| body.contains("FAKE-PROVISION-TOKEN:"))
    }

    fn parse(&self, mail: &ParsedMail) -> Result<ProvisioningEmailEvent, ProvisioningError> {
        let body = mail.text_plain.as_deref().unwrap_or_default();
        let token = body
            .split_once("FAKE-PROVISION-TOKEN:")
            .map(|(_, value)| value.lines().next().unwrap_or_default().trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ProvisioningError::Mail("fake verification token is missing".to_owned())
            })?;
        Ok(ProvisioningEmailEvent::VerificationCodeReceived { token })
    }
}

pub struct MegaProvisioningEmailParser;

impl ProvisioningEmailParser for MegaProvisioningEmailParser {
    fn provider_type(&self) -> &str {
        "mega"
    }

    fn matches(&self, _mail: &ParsedMail) -> bool {
        false
    }

    fn parse(&self, _mail: &ParsedMail) -> Result<ProvisioningEmailEvent, ProvisioningError> {
        Err(ProvisioningError::Mail(
            "MEGA email automation is intentionally not implemented".to_owned(),
        ))
    }
}

pub fn parse_mime(raw: &[u8], max_bytes: usize) -> Result<ParsedMail, ProvisioningError> {
    if max_bytes > 0 && raw.len() > max_bytes {
        return Err(ProvisioningError::Mail(format!(
            "inbound message exceeds configured limit of {max_bytes} bytes"
        )));
    }
    let (header_bytes, body_bytes) = split_headers_body(raw).ok_or_else(|| {
        ProvisioningError::Mail("MIME message has no header/body separator".to_owned())
    })?;
    let headers = parse_headers(header_bytes)?;
    let mut parsed = ParsedMail {
        message_id: headers
            .get("message-id")
            .cloned()
            .map(|value| value.trim().to_owned()),
        from: headers
            .get("from")
            .cloned()
            .map(|value| decode_header_value(&value)),
        to: headers
            .get("to")
            .map(|value| extract_addresses(value))
            .unwrap_or_default(),
        subject: headers
            .get("subject")
            .cloned()
            .map(|value| decode_header_value(&value)),
        date: headers.get("date").cloned(),
        text_plain: None,
        text_html: None,
        headers: headers.clone(),
    };
    parse_part(&headers, body_bytes, &mut parsed, 0)?;
    Ok(parsed)
}

fn split_headers_body(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (&raw[..index], &raw[index + 4..]))
        .or_else(|| {
            raw.windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (&raw[..index], &raw[index + 2..]))
        })
}

fn parse_headers(raw: &[u8]) -> Result<BTreeMap<String, String>, ProvisioningError> {
    let text = String::from_utf8_lossy(raw);
    let mut values = BTreeMap::new();
    let mut current_name: Option<String> = None;
    let mut current_value = String::new();
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if current_name.is_some() {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }
        if let Some(name) = current_name.take() {
            values.insert(name, current_value.trim().to_owned());
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| ProvisioningError::Mail("malformed MIME header".to_owned()))?;
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            return Err(ProvisioningError::Mail(
                "MIME header name is empty".to_owned(),
            ));
        }
        current_name = Some(name);
        current_value = value.trim().to_owned();
    }
    if let Some(name) = current_name {
        values.insert(name, current_value.trim().to_owned());
    }
    Ok(values)
}

fn parse_part(
    headers: &BTreeMap<String, String>,
    body: &[u8],
    parsed: &mut ParsedMail,
    depth: usize,
) -> Result<(), ProvisioningError> {
    if depth > 12 {
        return Err(ProvisioningError::Mail(
            "MIME nesting is too deep".to_owned(),
        ));
    }
    let content_type = headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| "text/plain".to_owned());
    if content_type.starts_with("multipart/") {
        let boundary = parameter(&content_type, "boundary").ok_or_else(|| {
            ProvisioningError::Mail("multipart message has no boundary".to_owned())
        })?;
        for part in split_multipart(body, &boundary) {
            let Some((part_headers, part_body)) = split_headers_body(&part) else {
                continue;
            };
            let part_headers = parse_headers(part_headers)?;
            parse_part(&part_headers, part_body, parsed, depth + 1)?;
        }
        return Ok(());
    }
    if !content_type.starts_with("text/plain") && !content_type.starts_with("text/html") {
        return Ok(());
    }
    let decoded = decode_transfer_encoding(
        body,
        headers.get("content-transfer-encoding").map(String::as_str),
    )?;
    let text = String::from_utf8_lossy(&decoded).trim().to_owned();
    if text.is_empty() {
        return Ok(());
    }
    if content_type.starts_with("text/html") {
        parsed.text_html = Some(match parsed.text_html.take() {
            Some(existing) => format!("{existing}\n{text}"),
            None => text,
        });
    } else {
        parsed.text_plain = Some(match parsed.text_plain.take() {
            Some(existing) => format!("{existing}\n{text}"),
            None => text,
        });
    }
    Ok(())
}

fn split_multipart(body: &[u8], boundary: &str) -> Vec<Vec<u8>> {
    let marker = format!("--{boundary}");
    let text = String::from_utf8_lossy(body);
    let mut parts = Vec::new();
    for segment in text.split(&marker).skip(1) {
        if segment.starts_with("--") {
            break;
        }
        let segment = segment.trim_start_matches(['\r', '\n']);
        let segment = segment.trim_end_matches(['\r', '\n']);
        if !segment.is_empty() {
            parts.push(segment.as_bytes().to_vec());
        }
    }
    parts
}

fn decode_transfer_encoding(
    body: &[u8],
    encoding: Option<&str>,
) -> Result<Vec<u8>, ProvisioningError> {
    match encoding
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "base64" => STANDARD
            .decode(
                body.iter()
                    .copied()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .collect::<Vec<_>>(),
            )
            .map_err(|_| ProvisioningError::Mail("invalid base64 MIME body".to_owned())),
        "quoted-printable" => decode_quoted_printable(body),
        _ => Ok(body.to_vec()),
    }
}

fn decode_quoted_printable(body: &[u8]) -> Result<Vec<u8>, ProvisioningError> {
    let mut output = Vec::with_capacity(body.len());
    let mut index = 0;
    while index < body.len() {
        if body[index] == b'=' {
            if index + 2 < body.len() && body[index + 1] == b'\r' && body[index + 2] == b'\n' {
                index += 3;
                continue;
            }
            if index + 1 < body.len() && body[index + 1] == b'\n' {
                index += 2;
                continue;
            }
            if index + 2 >= body.len() {
                return Err(ProvisioningError::Mail(
                    "truncated quoted-printable body".to_owned(),
                ));
            }
            let high = hex_value(body[index + 1])?;
            let low = hex_value(body[index + 2])?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(body[index]);
            index += 1;
        }
    }
    Ok(output)
}

fn hex_value(value: u8) -> Result<u8, ProvisioningError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(ProvisioningError::Mail(
            "invalid quoted-printable escape".to_owned(),
        )),
    }
}

fn parameter(content_type: &str, name: &str) -> Option<String> {
    content_type
        .split(';')
        .skip(1)
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| {
            (key.trim().eq_ignore_ascii_case(name))
                .then(|| value.trim().trim_matches('"').to_owned())
        })
}

fn extract_addresses(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            let address = part
                .rsplit_once('<')
                .and_then(|(_, rest)| rest.split_once('>').map(|(email, _)| email))
                .unwrap_or(part)
                .trim()
                .to_ascii_lowercase();
            (!address.is_empty() && address.contains('@')).then_some(address)
        })
        .collect()
}

fn decode_header_value(value: &str) -> String {
    let mut output = String::new();
    let mut remaining = value;
    while let Some(start) = remaining.find("=?") {
        output.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];
        let Some(end) = after.find("?=") else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let encoded = &after[..end];
        let mut pieces = encoded.splitn(3, '?');
        let _charset = pieces.next();
        let encoding = pieces.next();
        let data = pieces.next();
        let decoded = match (encoding, data) {
            (Some(encoding), Some(data)) if encoding.eq_ignore_ascii_case("b") => {
                decode_header_base64(data)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            }
            (Some(encoding), Some(data)) if encoding.eq_ignore_ascii_case("q") => {
                decode_quoted_printable(data.replace('_', " ").as_bytes())
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            }
            _ => None,
        };
        if let Some(decoded) = decoded {
            output.push_str(&decoded);
        } else {
            output.push_str(&remaining[start..start + 2 + end + 2]);
        }
        remaining = &after[end + 2..];
    }
    output.push_str(remaining);
    output.trim().to_owned()
}

fn decode_header_base64(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let padding = (4 - value.len() % 4) % 4;
    let mut padded = value.to_owned();
    padded.extend(std::iter::repeat_n('=', padding));
    STANDARD.decode(padded)
}

impl FromStr for EmailIngestHeaders {
    type Err = ProvisioningError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut fields = BTreeMap::new();
        for item in value.split(';') {
            if let Some((key, value)) = item.split_once('=') {
                fields.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        Ok(Self {
            timestamp: fields
                .get("timestamp")
                .ok_or_else(|| ProvisioningError::Security("missing timestamp".to_owned()))?
                .parse()
                .map_err(|_| ProvisioningError::Security("invalid timestamp".to_owned()))?,
            nonce: fields
                .remove("nonce")
                .ok_or_else(|| ProvisioningError::Security("missing nonce".to_owned()))?,
            signature: fields
                .remove("signature")
                .ok_or_else(|| ProvisioningError::Security("missing signature".to_owned()))?,
            envelope_from: fields.remove("from"),
            envelope_to: fields
                .remove("to")
                .ok_or_else(|| ProvisioningError::Security("missing recipient".to_owned()))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_html_multipart_and_encoded_subject() {
        let raw = concat!(
            "Message-ID: <fixture@example.test>\r\n",
            "From: Sender <sender@example.test>\r\n",
            "To: p@example.test, other@example.test\r\n",
            "Subject: =?UTF-8?B?RmFrZSB2ZXJpZmljYXRpb24?=\r\n",
            "Content-Type: multipart/alternative; boundary=abc\r\n\r\n",
            "--abc\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nFAKE-PROVISION-TOKEN: fake-=\r\ntoken\r\n",
            "--abc\r\nContent-Type: text/html\r\nContent-Transfer-Encoding: base64\r\n\r\nPGI+dmVyaWZ5PC9iPg==\r\n",
            "--abc--\r\n"
        );
        let parsed = parse_mime(raw.as_bytes(), 4096).unwrap();
        assert_eq!(parsed.message_id.as_deref(), Some("<fixture@example.test>"));
        assert_eq!(parsed.to, vec!["p@example.test", "other@example.test"]);
        assert_eq!(parsed.subject.as_deref(), Some("Fake verification"));
        assert!(parsed.text_plain.unwrap().contains("fake-token"));
        assert!(parsed.text_html.unwrap().contains("<b>verify</b>"));
    }

    #[test]
    fn hmac_vectors_reject_replay_material_changes() {
        let body = b"raw message";
        let timestamp = 1_700_000_000;
        let signature = compute_email_hmac(
            b"shared-secret",
            timestamp,
            "nonce-1",
            Some("sender@example.test"),
            "p@example.test",
            body,
        )
        .unwrap();
        let headers = EmailIngestHeaders {
            timestamp,
            nonce: "nonce-1".to_owned(),
            signature,
            envelope_from: Some("sender@example.test".to_owned()),
            envelope_to: "p@example.test".to_owned(),
        };
        assert!(
            verify_email_ingest(
                &headers,
                b"shared-secret",
                body,
                DateTime::<Utc>::from_timestamp(timestamp, 0).unwrap(),
                Duration::minutes(5),
            )
            .is_ok()
        );
        let mut modified = headers.clone();
        modified.envelope_to = "other@example.test".to_owned();
        assert!(
            verify_email_ingest(
                &modified,
                b"shared-secret",
                body,
                DateTime::<Utc>::from_timestamp(timestamp, 0).unwrap(),
                Duration::minutes(5),
            )
            .is_err()
        );
        let mut modified_body = headers.clone();
        modified_body.signature = compute_email_hmac(
            b"shared-secret",
            timestamp,
            "nonce-1",
            Some("sender@example.test"),
            "p@example.test",
            b"different body",
        )
        .unwrap();
        assert!(
            verify_email_ingest(
                &modified_body,
                b"shared-secret",
                body,
                DateTime::<Utc>::from_timestamp(timestamp, 0).unwrap(),
                Duration::minutes(5),
            )
            .is_err()
        );
        let future = DateTime::<Utc>::from_timestamp(timestamp + 301, 0).unwrap();
        assert!(
            verify_email_ingest(
                &headers,
                b"shared-secret",
                body,
                future,
                Duration::minutes(5)
            )
            .is_err()
        );
        let past = DateTime::<Utc>::from_timestamp(timestamp - 301, 0).unwrap();
        assert!(
            verify_email_ingest(&headers, b"shared-secret", body, past, Duration::minutes(5))
                .is_err()
        );
        let mut invalid_signature = headers.clone();
        invalid_signature.signature = "not-base64".to_owned();
        assert!(
            verify_email_ingest(
                &invalid_signature,
                b"shared-secret",
                body,
                DateTime::<Utc>::from_timestamp(timestamp, 0).unwrap(),
                Duration::minutes(5),
            )
            .is_err()
        );
        assert!(parse_mime(body, body.len() - 1).is_err());
    }
}
