use super::super::{
    CMD_PROPERTIES, Command, Payload, REPLY_DONE, Reply, decode_command, decode_reply,
    encode_command, encode_reply,
};
use super::*;

fn metadata() -> DocumentMetadata {
    DocumentMetadata {
        title: Some("文件內容 Properties".into()),
        author: None,
        subject: Some(String::new()),
        keywords: Some("屬性".into()),
        creator: None,
        producer: Some("FastPDF".into()),
        creation_date: Some("D:20261024153000-00'30'".into()),
        modification_date: None,
        pdf_version: Some("1.7".into()),
        encrypted: true,
    }
}

fn rich() -> DocumentProperties {
    DocumentProperties {
        metadata: metadata(),
        security: Some(Security {
            handler: Some("Standard".into()),
            method: EncryptionMethod::Rc4 { key_bits: 128 },
            version: Some(2),
            revision: Some(3),
            permissions: Some(Permissions::from_p(-3904, 3)),
            encrypt_metadata: false,
            user_password: true,
        }),
        forms: FormSummary {
            fields: 7,
            signature_fields: 3,
            signatures: 2,
            xfa: true,
            truncated: true,
        },
        tagged: true,
        linearized: true,
    }
}

/// The reply payload (without the frame length) carrying `p`.
fn reply_bytes(p: DocumentProperties) -> Vec<u8> {
    encode_reply(&Reply::Done {
        id: 3,
        result: Ok(Payload::Properties(Box::new(p))),
    })[4..]
        .to_vec()
}

fn decoded(bytes: &[u8]) -> Result<DocumentProperties> {
    match decode_reply(bytes)? {
        Reply::Done {
            result: Ok(Payload::Properties(p)),
            ..
        } => Ok(*p),
        other => panic!("not properties: {other:?}"),
    }
}

/// Where the security block starts in `reply_bytes(p)`: version (2) +
/// kind (1) + id (8) + result tag (1) + payload tag (1), then the
/// metadata.
fn security_at(p: &DocumentProperties) -> usize {
    let mut e = Encoder::new(0);
    put_metadata(&mut e, &p.metadata);
    13 + e.payload_len() - 3
}

#[test]
fn properties_round_trip() {
    for p in [
        rich(),
        DocumentProperties::default(),
        DocumentProperties {
            security: Some(Security {
                handler: None,
                method: EncryptionMethod::Unknown,
                version: None,
                revision: None,
                permissions: None,
                encrypt_metadata: true,
                user_password: false,
            }),
            ..DocumentProperties::default()
        },
    ] {
        assert_eq!(decoded(&reply_bytes(p.clone())), Ok(p));
    }
    for method in [
        EncryptionMethod::Rc4 { key_bits: 40 },
        EncryptionMethod::Aes128,
        EncryptionMethod::Aes256,
        EncryptionMethod::Identity,
    ] {
        let mut p = rich();
        if let Some(s) = &mut p.security {
            s.method = method;
        }
        assert_eq!(decoded(&reply_bytes(p.clone())), Ok(p));
    }
    let cmd = Command::Properties { id: 42 };
    let frame = encode_command(&cmd).unwrap_or_default();
    assert_eq!(decode_command(&frame[4..]), Ok(cmd));
}

#[test]
fn metadata_reads_the_same_in_both_payloads() {
    let metadata = metadata();
    let alone = encode_reply(&Reply::Done {
        id: 3,
        result: Ok(Payload::Metadata(metadata.clone())),
    });
    let properties = reply_bytes(DocumentProperties::from_metadata(metadata));
    // Frame length (4), then everything up to the payload tag differs only
    // in that tag; the metadata bytes that follow are the same.
    let metadata_bytes = &alone[4 + 13..];
    assert_eq!(&properties[13..13 + metadata_bytes.len()], metadata_bytes);
    // The metadata payload as written before properties existed: nine
    // optional strings, then the flag.
    let mut expected = Vec::new();
    for value in [
        Some("文件內容 Properties"),
        None,
        Some(""),
        Some("屬性"),
        None,
        Some("FastPDF"),
        Some("D:20261024153000-00'30'"),
        None,
        Some("1.7"),
    ] {
        match value {
            Some(s) => {
                expected.push(1);
                expected.extend_from_slice(&(s.len() as u32).to_le_bytes());
                expected.extend_from_slice(s.as_bytes());
            }
            None => expected.push(0),
        }
    }
    expected.push(1);
    assert_eq!(metadata_bytes, expected.as_slice());
}

#[test]
fn properties_are_rejected_in_the_wrong_direction() {
    let frame = encode_command(&Command::Properties { id: 1 }).unwrap_or_default();
    assert_eq!(
        decode_reply(&frame[4..]),
        Err(ProtocolError::UnknownKind(CMD_PROPERTIES))
    );
    assert_eq!(
        decode_command(&reply_bytes(rich())),
        Err(ProtocolError::UnknownKind(REPLY_DONE))
    );
}

#[test]
fn invalid_properties_are_rejected() {
    let p = rich();
    let good = reply_bytes(p.clone());
    let at = security_at(&p);
    assert_eq!(good[at], 1, "security present");
    // presence (1) + handler: present (1) + length (4) + "Standard" (8)
    let method_at = at + 1 + 1 + 4 + 8;
    assert_eq!(good[method_at], 0, "RC4");

    let mut bad = good.clone();
    bad[method_at] = 9;
    assert_eq!(
        decoded(&bad),
        Err(ProtocolError::BadTag("encryption method", 9))
    );
    let mut bad = good.clone();
    bad[at] = 2;
    assert_eq!(decoded(&bad), Err(ProtocolError::BadTag("security", 2)));

    // The last four bytes: xfa, truncated, tagged, linearized.
    let end = good.len();
    for (offset, what) in [
        (4, "xfa"),
        (3, "form truncated"),
        (2, "tagged"),
        (1, "linearized"),
    ] {
        let mut bad = good.clone();
        bad[end - offset] = 2;
        assert_eq!(decoded(&bad), Err(ProtocolError::BadTag(what, 2)), "{what}");
    }

    // Counts that cannot be: more signed than signature fields, more
    // signature fields than fields.
    let counts_at = end - 4 - 12;
    for (index, value) in [(2usize, 4u32), (1, 8)] {
        let mut bad = good.clone();
        let i = counts_at + 4 * index;
        bad[i..i + 4].copy_from_slice(&value.to_le_bytes());
        assert_eq!(decoded(&bad), Err(ProtocolError::BadValue("form counts")));
    }

    // A handler name over the cap.
    let mut long = rich();
    if let Some(s) = &mut long.security {
        s.handler = Some("h".repeat(MAX_HANDLER));
    }
    let mut bad = reply_bytes(long);
    let len_at = at + 2;
    bad[len_at..len_at + 4].copy_from_slice(&(MAX_HANDLER as u32 + 1).to_le_bytes());
    bad.insert(len_at + 4, b'h');
    assert_eq!(
        decoded(&bad),
        Err(ProtocolError::TooLong("security handler"))
    );

    // Every truncation fails, and trailing bytes too.
    for cut in 0..good.len() {
        assert!(decoded(&good[..cut]).is_err(), "cut at {cut}");
    }
    let mut longer = good;
    longer.push(0);
    assert_eq!(decoded(&longer), Err(ProtocolError::TrailingBytes));
}

#[test]
fn engine_properties_are_sanitized_not_rejected() {
    let mut p = rich();
    p.forms = FormSummary {
        fields: 2,
        signature_fields: 5,
        signatures: 9,
        xfa: false,
        truncated: false,
    };
    if let Some(s) = &mut p.security {
        s.handler = Some("測".repeat(40)); // 120 bytes
    }
    let back = decoded(&reply_bytes(p)).unwrap_or_default();
    assert_eq!(
        (
            back.forms.fields,
            back.forms.signature_fields,
            back.forms.signatures
        ),
        (2, 2, 2)
    );
    let handler = back.security.and_then(|s| s.handler).unwrap_or_default();
    assert!(handler.len() <= MAX_HANDLER && !handler.is_empty());
    assert!(handler.chars().all(|c| c == '測'));
}
