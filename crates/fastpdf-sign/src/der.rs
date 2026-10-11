//! The little DER the signature needs that Windows does not write itself:
//! the ESS signing-certificate-v2 attribute (RFC 5035).

const SEQUENCE: u8 = 0x30;
const OCTET_STRING: u8 = 0x04;
const INTEGER: u8 = 0x02;
/// `directoryName [4]` of `GeneralName`: explicit, because `Name` is a
/// CHOICE.
const DIRECTORY_NAME: u8 = 0xA4;

/// A DER element: tag, definite length, content.
fn element(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 6);
    out.push(tag);
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(content);
    out
}

/// An INTEGER from its big-endian two's complement bytes, in the shortest
/// form DER allows.
fn integer(big_endian: &[u8]) -> Vec<u8> {
    let mut content = big_endian;
    // Leading 0x00 that only repeat the sign of the next byte go.
    while content.len() > 1 && content[0] == 0 && content[1] & 0x80 == 0 {
        content = &content[1..];
    }
    let mut bytes = Vec::with_capacity(content.len() + 1);
    if content.is_empty() {
        bytes.push(0);
    } else if content[0] & 0x80 != 0 {
        // A serial number is positive.
        bytes.push(0);
    }
    bytes.extend_from_slice(content);
    element(INTEGER, &bytes)
}

/// The value of the ESS `signing-certificate-v2` attribute (OID
/// 1.2.840.113549.1.9.16.2.47) for a certificate: its SHA-256 hash (the
/// default algorithm, so not written) and its issuer and serial number.
///
/// ```text
/// SigningCertificateV2 ::= SEQUENCE { certs SEQUENCE OF ESSCertIDv2 }
/// ESSCertIDv2 ::= SEQUENCE { certHash OCTET STRING, issuerSerial IssuerSerial }
/// IssuerSerial ::= SEQUENCE { issuer GeneralNames, serialNumber INTEGER }
/// ```
///
/// `issuer` is the DER `Name` of the issuer; `serial` the serial number in
/// big-endian order.
pub fn signing_certificate_v2(cert_sha256: &[u8; 32], issuer: &[u8], serial: &[u8]) -> Vec<u8> {
    let general_names = element(SEQUENCE, &element(DIRECTORY_NAME, issuer));
    let issuer_serial = element(SEQUENCE, &[general_names, integer(serial)].concat());
    let cert_id = element(
        SEQUENCE,
        &[element(OCTET_STRING, cert_sha256), issuer_serial].concat(),
    );
    element(SEQUENCE, &element(SEQUENCE, &cert_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_use_the_short_and_long_forms() {
        assert_eq!(element(0x04, &[1, 2]), [0x04, 2, 1, 2]);
        let long = element(0x04, &[7; 200]);
        assert_eq!(&long[..3], &[0x04, 0x81, 200]);
        assert_eq!(long.len(), 203);
        let longer = element(0x04, &vec![0; 0x1234]);
        assert_eq!(&longer[..4], &[0x04, 0x82, 0x12, 0x34]);
    }

    #[test]
    fn integers_are_minimal_and_positive() {
        assert_eq!(integer(&[0x01]), [0x02, 1, 0x01]);
        assert_eq!(integer(&[0x80]), [0x02, 2, 0x00, 0x80]);
        assert_eq!(integer(&[0x00, 0x00, 0x7F]), [0x02, 1, 0x7F]);
        assert_eq!(integer(&[0x00, 0x80]), [0x02, 2, 0x00, 0x80]);
        assert_eq!(integer(&[]), [0x02, 1, 0x00]);
    }

    #[test]
    fn the_attribute_nests_as_rfc_5035_says() {
        // Issuer "CN=A": SEQUENCE { SET { SEQUENCE { OID 2.5.4.3, UTF8 "A" } } }
        let issuer = [
            0x30, 0x0C, 0x31, 0x0A, 0x30, 0x08, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0C, 0x01, b'A',
        ];
        let value = signing_certificate_v2(&[0x11; 32], &issuer, &[0x05]);
        // IssuerSerial: GeneralNames { [4] { Name } }, INTEGER 5.
        let issuer_serial = element(
            SEQUENCE,
            &[
                element(SEQUENCE, &element(DIRECTORY_NAME, &issuer)),
                integer(&[5]),
            ]
            .concat(),
        );
        assert_eq!(issuer_serial[..2], [0x30, 0x15]);
        assert_eq!(issuer_serial[2..6], [0x30, 0x10, 0xA4, 0x0E]);
        assert_eq!(value[..2], [0x30, (value.len() - 2) as u8]);
        assert_eq!(value[2..4], [0x30, (value.len() - 4) as u8]);
        assert_eq!(value[4..6], [0x30, (value.len() - 6) as u8]);
        assert_eq!(value[6..8], [0x04, 0x20]);
        assert!(value.ends_with(&issuer_serial));
        assert_eq!(value.len(), 6 + 34 + issuer_serial.len());
    }
}
