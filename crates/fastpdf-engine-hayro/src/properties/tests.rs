use super::*;

fn method_of(dict: &[u8]) -> EncryptionMethod {
    let dict = Dict::from_bytes(dict).unwrap_or_default();
    let version = dict.get::<i64>(b"V").and_then(|v| u8::try_from(v).ok());
    method(&dict, version)
}

#[test]
fn crypt_filters_name_the_cipher_and_its_key_length() {
    let rc4 = |cf: &str, extra: &str| {
        method_of(
            format!(
                "<< /V 4 /StmF /StdCF /StrF /StdCF /CF << /StdCF << /CFM /V2 {cf} >> >> {extra} >>"
            )
            .as_bytes(),
        )
    };
    // Bytes as the standard security handler writes them, or bits.
    assert_eq!(rc4("/Length 5", ""), EncryptionMethod::Rc4 { key_bits: 40 });
    assert_eq!(
        rc4("/Length 16", ""),
        EncryptionMethod::Rc4 { key_bits: 128 }
    );
    assert_eq!(
        rc4("/Length 128", ""),
        EncryptionMethod::Rc4 { key_bits: 128 }
    );
    assert_eq!(
        rc4("/Length 56", ""),
        EncryptionMethod::Rc4 { key_bits: 56 }
    );
    // Otherwise the dictionary's length, otherwise 128.
    assert_eq!(
        rc4("", "/Length 40"),
        EncryptionMethod::Rc4 { key_bits: 40 }
    );
    assert_eq!(
        rc4("/Length 3", ""),
        EncryptionMethod::Rc4 { key_bits: 128 }
    );
    assert_eq!(rc4("", ""), EncryptionMethod::Rc4 { key_bits: 128 });
    assert_eq!(
        method_of(b"<< /V 4 /StmF /StdCF /CF << /StdCF << /CFM /AESV2 >> >> >>"),
        EncryptionMethod::Aes128
    );
    assert_eq!(
        method_of(b"<< /V 5 /StmF /StdCF /CF << /StdCF << /CFM /AESV3 >> >> >>"),
        EncryptionMethod::Aes256
    );
    // Streams not encrypted: the strings' filter says.
    assert_eq!(
        method_of(b"<< /V 4 /StmF /Identity /StrF /S /CF << /S << /CFM /AESV2 >> >> >>"),
        EncryptionMethod::Aes128
    );
    assert_eq!(method_of(b"<< /V 4 >>"), EncryptionMethod::Identity);
    assert_eq!(
        method_of(b"<< /V 1 >>"),
        EncryptionMethod::Rc4 { key_bits: 40 }
    );
    assert_eq!(
        method_of(b"<< /V 2 /Length 128 >>"),
        EncryptionMethod::Rc4 { key_bits: 128 }
    );
    assert_eq!(method_of(b"<< /V 3 >>"), EncryptionMethod::Unknown);
    assert_eq!(
        method_of(b"<< /V 4 /StmF /X /CF << /X << /CFM /Other >> >> >>"),
        EncryptionMethod::Unknown
    );
}

#[test]
fn offsets_hayro_cannot_know_are_dropped() {
    let date = |text: &str| {
        let mut d = text.to_owned();
        drop_unknown_offset(&mut d);
        d
    };
    assert_eq!(date("D:20261024153000+00'00'"), "D:20261024153000");
    assert_eq!(date("D:20261024153000+00'30'"), "D:20261024153000");
    assert_eq!(date("D:20261024153000+08'00'"), "D:20261024153000+08'00'");
    assert_eq!(date("D:20261024153000-05'00'"), "D:20261024153000-05'00'");
    assert_eq!(date("D:2026"), "D:2026");
    assert_eq!(date(""), "");
}

#[test]
fn linearization_names_the_file_length() {
    let doc = |len: &str| {
        format!("%PDF-1.7\n1 0 obj\n<< /Linearized 1 /L {len} /O 3 >>\nendobj\n%%EOF\n")
    };
    let probe = doc("0000");
    let exact = doc(&format!("{:04}", probe.len()));
    assert!(linearized(exact.as_bytes()));
    assert!(!linearized(probe.as_bytes()));
    let mut appended = exact.clone().into_bytes();
    appended.extend_from_slice(b"% update\n");
    assert!(!linearized(&appended));
    // After the first 1024 bytes it does not count.
    let late = format!("%PDF-1.7\n%{}\n{}", "x".repeat(1100), &exact[9..]);
    assert!(!linearized(late.as_bytes()));
    assert!(!linearized(b""));
    assert!(!linearized(b"/Linearized <<"));
}
