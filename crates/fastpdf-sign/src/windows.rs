//! The Windows side: the current user's personal certificate store,
//! crypt32's CMS encoder and, for tests and development scripts, a key that
//! lives only as long as one signature.
//!
//! The store is opened read-only; nothing here adds, changes or removes
//! certificates or keys anywhere. The ephemeral signer's key is a CNG key
//! without a name (never persisted) and its certificate exists only in
//! memory.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{FILETIME, GetLastError, SYSTEMTIME};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_RSA_ALGORITHM, BCRYPT_SHA256_ALGORITHM, CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL,
    CERT_CHAIN_CONTEXT, CERT_CHAIN_DISABLE_AIA, CERT_CHAIN_DISABLE_AUTH_ROOT_AUTO_UPDATE,
    CERT_CHAIN_PARA, CERT_CONTEXT, CERT_CREATE_SELFSIGN_NO_KEY_INFO,
    CERT_DIGITAL_SIGNATURE_KEY_USAGE, CERT_FIND_SHA1_HASH, CERT_INFO, CERT_KEY_PROV_INFO_PROP_ID,
    CERT_NAME_ISSUER_FLAG, CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_NCRYPT_KEY_SPEC,
    CERT_NON_REPUDIATION_KEY_USAGE, CERT_SHA1_HASH_PROP_ID, CERT_STORE_OPEN_EXISTING_FLAG,
    CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER,
    CERT_X500_NAME_STR, CMSG_CONTENT_PARAM, CMSG_DETACHED_FLAG, CMSG_SIGNED,
    CMSG_SIGNED_ENCODE_INFO, CMSG_SIGNER_ENCODE_INFO, CMSG_SIGNER_ENCODE_INFO_0,
    CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG, CRYPT_ACQUIRE_WINDOW_HANDLE_FLAG,
    CRYPT_ALGORITHM_IDENTIFIER, CRYPT_ATTRIBUTE, CRYPT_INTEGER_BLOB, CRYPT_VERIFY_MESSAGE_PARA,
    CertCloseStore, CertCompareCertificateName, CertCreateSelfSignCertificate,
    CertEnumCertificatesInStore, CertFindCertificateInStore, CertFreeCertificateChain,
    CertFreeCertificateContext, CertGetCertificateChain, CertGetCertificateContextProperty,
    CertGetIntendedKeyUsage, CertGetNameStringW, CertOpenStore, CertStrToNameW,
    CertVerifyTimeValidity, CryptAcquireCertificatePrivateKey, CryptHashCertificate2,
    CryptMsgClose, CryptMsgGetParam, CryptMsgOpenToEncode, CryptMsgUpdate, CryptReleaseContext,
    CryptVerifyDetachedMessageSignature, HCERTSTORE, MS_KEY_STORAGE_PROVIDER,
    NCRYPT_LENGTH_PROPERTY, NCryptCreatePersistedKey, NCryptFinalizeKey, NCryptFreeObject,
    NCryptOpenStorageProvider, NCryptSetProperty, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};
use windows_sys::Win32::System::Time::FileTimeToSystemTime;

use crate::{Certificate, SignError, Signer, signing_certificate_v2};

const ENCODING: u32 = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
/// ESS signing-certificate-v2 (RFC 5035).
const OID_SIGNING_CERTIFICATE_V2: &[u8] = b"1.2.840.113549.1.9.16.2.47\0";
const OID_SHA256: &[u8] = b"2.16.840.1.101.3.4.2.1\0";
const OID_SHA256_WITH_RSA: &[u8] = b"1.2.840.113549.1.1.11\0";
const EPHEMERAL_SUBJECT: &str = "CN=FastPDF Test Signer, O=FastPDF";

/// Error codes that mean the user or the card said no.
const ERROR_CANCELLED: u32 = 1223;
const HRESULT_CANCELLED: u32 = 0x8007_04C7;
const NTE_USER_CANCELLED: u32 = 0x8009_0036;
const SCARD_W_CANCELLED_BY_USER: u32 = 0x8010_006E;
const SCARD_W_WRONG_CHV: u32 = 0x8010_006B;
const SCARD_W_CHV_BLOCKED: u32 = 0x8010_006C;
const SCARD_E_NO_SMARTCARD: u32 = 0x8010_000C;
const SCARD_W_REMOVED_CARD: u32 = 0x8010_0069;
const SCARD_E_NO_READERS_AVAILABLE: u32 = 0x8010_002E;
const NTE_BAD_KEYSET: u32 = 0x8009_0016;

fn classify(what: &'static str, code: u32) -> SignError {
    match code {
        ERROR_CANCELLED | HRESULT_CANCELLED | NTE_USER_CANCELLED | SCARD_W_CANCELLED_BY_USER => {
            SignError::Cancelled
        }
        SCARD_W_WRONG_CHV | SCARD_W_CHV_BLOCKED => SignError::WrongPin,
        SCARD_E_NO_SMARTCARD
        | SCARD_W_REMOVED_CARD
        | SCARD_E_NO_READERS_AVAILABLE
        | NTE_BAD_KEYSET => SignError::CardMissing,
        _ => SignError::Failed { what, code },
    }
}

/// The last Win32 error, classified.
fn last_error(what: &'static str) -> SignError {
    // SAFETY: reads the calling thread's last-error value.
    classify(what, unsafe { GetLastError() })
}

fn check_hresult(what: &'static str, hr: i32) -> Result<(), SignError> {
    if hr < 0 {
        Err(classify(what, hr as u32))
    } else {
        Ok(())
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The bytes of a blob.
///
/// SAFETY: `blob` must describe readable memory that outlives the slice.
unsafe fn blob<'a>(blob: &CRYPT_INTEGER_BLOB) -> &'a [u8] {
    if blob.pbData.is_null() || blob.cbData == 0 {
        return &[];
    }
    // SAFETY: the caller guarantees `pbData` points to `cbData` bytes.
    unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }
}

/// The current user's personal store, read-only.
struct Store(HCERTSTORE);

impl Store {
    fn personal() -> Option<Self> {
        let name = wide("MY");
        // SAFETY: `name` is a NUL-terminated wide string that outlives the
        // call; the store is opened read-only and must already exist.
        let handle = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                0,
                0,
                CERT_SYSTEM_STORE_CURRENT_USER
                    | CERT_STORE_READONLY_FLAG
                    | CERT_STORE_OPEN_EXISTING_FLAG,
                name.as_ptr().cast(),
            )
        };
        (!handle.is_null()).then_some(Self(handle))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // SAFETY: the handle came from CertOpenStore and is closed once.
        unsafe { CertCloseStore(self.0, 0) };
    }
}

/// A certificate context this code owns (one reference).
struct Cert(*const CERT_CONTEXT);

impl Cert {
    fn context(&self) -> &CERT_CONTEXT {
        // SAFETY: the pointer is a live context for as long as `self`.
        unsafe { &*self.0 }
    }

    fn info(&self) -> &CERT_INFO {
        // SAFETY: every certificate context has its decoded info.
        unsafe { &*self.context().pCertInfo }
    }

    fn encoded(&self) -> &[u8] {
        let c = self.context();
        // SAFETY: the encoded certificate lives as long as the context.
        unsafe { std::slice::from_raw_parts(c.pbCertEncoded, c.cbCertEncoded as usize) }
    }
}

impl Drop for Cert {
    fn drop(&mut self) {
        // SAFETY: releases the one reference this value holds.
        unsafe { CertFreeCertificateContext(self.0) };
    }
}

/// A property of a certificate context.
fn property(context: *const CERT_CONTEXT, id: u32) -> Option<Vec<u8>> {
    let mut size = 0u32;
    // SAFETY: a size query with a null buffer.
    if unsafe { CertGetCertificateContextProperty(context, id, null_mut(), &mut size) } == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: `data` has the `size` bytes asked for.
    let ok = unsafe {
        CertGetCertificateContextProperty(context, id, data.as_mut_ptr().cast(), &mut size)
    };
    if ok == 0 {
        return None;
    }
    data.truncate(size as usize);
    Some(data)
}

/// The display name of a certificate's subject (or issuer).
fn display_name(context: *const CERT_CONTEXT, issuer: bool) -> String {
    let flags = if issuer { CERT_NAME_ISSUER_FLAG } else { 0 };
    let mut buffer = vec![0u16; 256];
    // SAFETY: the buffer holds `len` wide characters; the function writes a
    // NUL-terminated name and returns its length including the NUL.
    let len = unsafe {
        CertGetNameStringW(
            context,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            flags,
            null(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    };
    let len = (len as usize).saturating_sub(1).min(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

fn date(time: FILETIME) -> String {
    let mut system = SYSTEMTIME::default();
    // SAFETY: converts one FILETIME into the given SYSTEMTIME.
    if unsafe { FileTimeToSystemTime(&time, &mut system) } == 0 {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02}",
        system.wYear, system.wMonth, system.wDay
    )
}

/// Whether a certificate can sign: a private key, valid now, and a key
/// usage (if it has one) that allows digital signatures.
fn describe(context: *const CERT_CONTEXT) -> Option<Certificate> {
    property(context, CERT_KEY_PROV_INFO_PROP_ID)?;
    // SAFETY: a context from the store has its decoded info.
    let info = unsafe { &*(*context).pCertInfo };
    // SAFETY: checks the info's validity period against the current time.
    if unsafe { CertVerifyTimeValidity(null(), info) } != 0 {
        return None;
    }
    let mut usage = [0u8; 2];
    // SAFETY: `usage` has room for the two bytes passed as its size.
    let has_usage = unsafe { CertGetIntendedKeyUsage(ENCODING, info, usage.as_mut_ptr(), 2) } != 0;
    let signing = (CERT_DIGITAL_SIGNATURE_KEY_USAGE | CERT_NON_REPUDIATION_KEY_USAGE) as u8;
    if has_usage && usage[0] & signing == 0 {
        return None;
    }
    let thumbprint: [u8; 20] = property(context, CERT_SHA1_HASH_PROP_ID)?.try_into().ok()?;
    Some(Certificate {
        thumbprint,
        subject: display_name(context, false),
        issuer: display_name(context, true),
        valid_until: date(info.NotAfter),
    })
}

pub(crate) fn certificates() -> Vec<Certificate> {
    let Some(store) = Store::personal() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut context: *const CERT_CONTEXT = null();
    loop {
        // SAFETY: passes back the previous context, which the call frees;
        // the loop runs until it returns null, so none is left behind.
        context = unsafe { CertEnumCertificatesInStore(store.0, context) };
        if context.is_null() {
            break;
        }
        if let Some(certificate) = describe(context) {
            out.push(certificate);
        }
    }
    out.sort_by(|a, b| {
        a.subject
            .cmp(&b.subject)
            .then(a.valid_until.cmp(&b.valid_until))
    });
    out
}

fn find(store: &Store, thumbprint: &[u8; 20]) -> Option<Cert> {
    let hash = CRYPT_INTEGER_BLOB {
        cbData: 20,
        pbData: thumbprint.as_ptr().cast_mut(),
    };
    // SAFETY: the hash blob points to 20 bytes that outlive the call.
    let context = unsafe {
        CertFindCertificateInStore(
            store.0,
            ENCODING,
            0,
            CERT_FIND_SHA1_HASH,
            (&raw const hash).cast(),
            null(),
        )
    };
    (!context.is_null()).then_some(Cert(context))
}

/// A private key handle: a CNG key or a legacy CSP.
struct Key {
    handle: usize,
    spec: u32,
    owned: bool,
}

impl Key {
    /// The private key of `cert`; `window` owns the PIN dialog.
    fn of(cert: &Cert, window: isize) -> Result<Self, SignError> {
        let mut handle = 0usize;
        let mut spec = 0u32;
        let mut owned = 0;
        let hwnd = window;
        let (flags, parameters) = if window != 0 {
            (
                CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_WINDOW_HANDLE_FLAG,
                (&raw const hwnd).cast::<c_void>(),
            )
        } else {
            (CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG, null())
        };
        // SAFETY: `parameters` points to the window handle (when the flag
        // says so) for the duration of the call; the out parameters are
        // valid.
        let ok = unsafe {
            CryptAcquireCertificatePrivateKey(
                cert.0,
                flags,
                parameters,
                &mut handle,
                &mut spec,
                &mut owned,
            )
        };
        if ok == 0 {
            return Err(last_error("CryptAcquireCertificatePrivateKey"));
        }
        Ok(Self {
            handle,
            spec,
            owned: owned != 0,
        })
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        // SAFETY: the handle is ours to free, of the kind `spec` says.
        unsafe {
            if self.spec == CERT_NCRYPT_KEY_SPEC {
                NCryptFreeObject(self.handle);
            } else {
                CryptReleaseContext(self.handle, 0);
            }
        }
    }
}

/// A CNG object (provider or key) freed on drop.
struct NcryptObject(usize);

impl Drop for NcryptObject {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the handle came from NCrypt and is freed once.
            unsafe { NCryptFreeObject(self.0) };
        }
    }
}

/// A key that is never persisted, and a self-signed certificate for it that
/// exists only in memory.
struct Ephemeral {
    // Field order: the certificate and key go before their provider.
    cert: Cert,
    key: NcryptObject,
    _provider: NcryptObject,
}

impl Ephemeral {
    fn new() -> Result<Self, SignError> {
        let mut provider = 0usize;
        // SAFETY: opens the software key storage provider.
        check_hresult("NCryptOpenStorageProvider", unsafe {
            NCryptOpenStorageProvider(&mut provider, MS_KEY_STORAGE_PROVIDER, 0)
        })?;
        let provider = NcryptObject(provider);
        let mut key = 0usize;
        // SAFETY: a null key name makes the key ephemeral: it is never
        // written to the key store.
        check_hresult("NCryptCreatePersistedKey", unsafe {
            NCryptCreatePersistedKey(provider.0, &mut key, BCRYPT_RSA_ALGORITHM, null(), 0, 0)
        })?;
        let key = NcryptObject(key);
        let bits: u32 = 2048;
        // SAFETY: sets the key length from a 4-byte value before finalizing.
        check_hresult("NCryptSetProperty", unsafe {
            NCryptSetProperty(
                key.0,
                NCRYPT_LENGTH_PROPERTY,
                (&raw const bits).cast(),
                4,
                0,
            )
        })?;
        // SAFETY: finalizes the (ephemeral) key.
        check_hresult("NCryptFinalizeKey", unsafe { NCryptFinalizeKey(key.0, 0) })?;

        let subject = wide(EPHEMERAL_SUBJECT);
        let mut size = 0u32;
        // SAFETY: a size query for the encoded name.
        let ok = unsafe {
            CertStrToNameW(
                X509_ASN_ENCODING,
                subject.as_ptr(),
                CERT_X500_NAME_STR,
                null(),
                null_mut(),
                &mut size,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error("CertStrToNameW"));
        }
        let mut name = vec![0u8; size as usize];
        // SAFETY: `name` has the `size` bytes asked for.
        let ok = unsafe {
            CertStrToNameW(
                X509_ASN_ENCODING,
                subject.as_ptr(),
                CERT_X500_NAME_STR,
                null(),
                name.as_mut_ptr(),
                &mut size,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error("CertStrToNameW"));
        }
        let name_blob = CRYPT_INTEGER_BLOB {
            cbData: size,
            pbData: name.as_mut_ptr(),
        };
        let algorithm = CRYPT_ALGORITHM_IDENTIFIER {
            pszObjId: OID_SHA256_WITH_RSA.as_ptr().cast_mut(),
            Parameters: CRYPT_INTEGER_BLOB::default(),
        };
        // SAFETY: the name and algorithm outlive the call; without key
        // information the certificate is not tied to any stored key and is
        // created in memory only.
        let context = unsafe {
            CertCreateSelfSignCertificate(
                key.0,
                &name_blob,
                CERT_CREATE_SELFSIGN_NO_KEY_INFO,
                null(),
                &algorithm,
                null(),
                null(),
                null(),
            )
        };
        if context.is_null() {
            return Err(last_error("CertCreateSelfSignCertificate"));
        }
        Ok(Self {
            cert: Cert(context),
            key,
            _provider: provider,
        })
    }
}

fn sha256(data: &[u8]) -> Result<[u8; 32], SignError> {
    let mut hash = [0u8; 32];
    let mut size = 32u32;
    // SAFETY: `hash` has the 32 bytes a SHA-256 hash needs.
    let ok = unsafe {
        CryptHashCertificate2(
            BCRYPT_SHA256_ALGORITHM,
            0,
            null(),
            data.as_ptr(),
            data.len() as u32,
            hash.as_mut_ptr(),
            &mut size,
        )
    };
    if ok == 0 || size != 32 {
        return Err(last_error("CryptHashCertificate2"));
    }
    Ok(hash)
}

/// The certificates the signature carries: the signer's, then the
/// intermediate ones Windows can find without the network (no roots).
fn chain(cert: &Cert) -> Vec<Vec<u8>> {
    let mut out = vec![cert.encoded().to_vec()];
    let para = CERT_CHAIN_PARA {
        cbSize: size_of::<CERT_CHAIN_PARA>() as u32,
        ..CERT_CHAIN_PARA::default()
    };
    let mut context: *mut CERT_CHAIN_CONTEXT = null_mut();
    let flags = CERT_CHAIN_CACHE_ONLY_URL_RETRIEVAL
        | CERT_CHAIN_DISABLE_AIA
        | CERT_CHAIN_DISABLE_AUTH_ROOT_AUTO_UPDATE;
    // SAFETY: builds a chain with the default engine from local stores and
    // caches only; `context` receives it.
    let ok = unsafe {
        CertGetCertificateChain(
            null_mut(),
            cert.0,
            null(),
            null_mut(),
            &para,
            flags,
            null(),
            &mut context,
        )
    };
    if ok == 0 || context.is_null() {
        return out;
    }
    // SAFETY: a chain context from CertGetCertificateChain: `rgpChain` has
    // `cChain` simple chains, each with `cElement` elements; it stays valid
    // until CertFreeCertificateChain below.
    unsafe {
        let chains = &*context;
        if chains.cChain > 0 && !chains.rgpChain.is_null() {
            let simple = &**chains.rgpChain;
            for i in 1..simple.cElement as usize {
                let element = &**simple.rgpElement.add(i);
                let issuer = element.pCertContext;
                let info = &*(*issuer).pCertInfo;
                if CertCompareCertificateName(ENCODING, &info.Subject, &info.Issuer) != 0 {
                    // A root: the verifier must have it already.
                    continue;
                }
                out.push(
                    std::slice::from_raw_parts(
                        (*issuer).pbCertEncoded,
                        (*issuer).cbCertEncoded as usize,
                    )
                    .to_vec(),
                );
            }
        }
        CertFreeCertificateChain(context);
    }
    out
}

/// A CMS message being encoded.
struct Message(*mut c_void);

impl Drop for Message {
    fn drop(&mut self) {
        // SAFETY: the handle came from CryptMsgOpenToEncode and is closed once.
        unsafe { CryptMsgClose(self.0) };
    }
}

/// The detached CMS signature of `parts` by `cert`'s key.
fn encode(
    cert: &Cert,
    key: usize,
    spec: u32,
    certificates: &[Vec<u8>],
    parts: &[&[u8]],
) -> Result<Vec<u8>, SignError> {
    let info = cert.info();
    let hash = sha256(cert.encoded())?;
    // SAFETY: the issuer and serial blobs belong to `cert`, alive here.
    let (issuer, serial) = unsafe { (blob(&info.Issuer), blob(&info.SerialNumber)) };
    // crypt32 keeps the serial number little-endian.
    let serial: Vec<u8> = serial.iter().rev().copied().collect();
    let mut value = signing_certificate_v2(&hash, issuer, &serial);
    let mut value_blob = CRYPT_INTEGER_BLOB {
        cbData: value.len() as u32,
        pbData: value.as_mut_ptr(),
    };
    let mut attribute = CRYPT_ATTRIBUTE {
        pszObjId: OID_SIGNING_CERTIFICATE_V2.as_ptr().cast_mut(),
        cValue: 1,
        rgValue: &mut value_blob,
    };
    let mut signer = CMSG_SIGNER_ENCODE_INFO {
        cbSize: size_of::<CMSG_SIGNER_ENCODE_INFO>() as u32,
        pCertInfo: std::ptr::from_ref(info).cast_mut(),
        Anonymous: CMSG_SIGNER_ENCODE_INFO_0 { hCryptProv: key },
        dwKeySpec: spec,
        HashAlgorithm: CRYPT_ALGORITHM_IDENTIFIER {
            pszObjId: OID_SHA256.as_ptr().cast_mut(),
            Parameters: CRYPT_INTEGER_BLOB::default(),
        },
        pvHashAuxInfo: null_mut(),
        // Content type and message digest are added by crypt32.
        cAuthAttr: 1,
        rgAuthAttr: &mut attribute,
        cUnauthAttr: 0,
        rgUnauthAttr: null_mut(),
    };
    let mut blobs: Vec<CRYPT_INTEGER_BLOB> = certificates
        .iter()
        .map(|c| CRYPT_INTEGER_BLOB {
            cbData: c.len() as u32,
            pbData: c.as_ptr().cast_mut(),
        })
        .collect();
    let signed = CMSG_SIGNED_ENCODE_INFO {
        cbSize: size_of::<CMSG_SIGNED_ENCODE_INFO>() as u32,
        cSigners: 1,
        rgSigners: &mut signer,
        cCertEncoded: blobs.len() as u32,
        rgCertEncoded: blobs.as_mut_ptr(),
        cCrlEncoded: 0,
        rgCrlEncoded: null_mut(),
    };
    // SAFETY: every pointer in `signed` (signer info, attribute, value,
    // certificates) points to data that lives until the message is closed
    // at the end of this function.
    let handle = unsafe {
        CryptMsgOpenToEncode(
            ENCODING,
            CMSG_DETACHED_FLAG,
            CMSG_SIGNED,
            (&raw const signed).cast(),
            null(),
            null(),
        )
    };
    if handle.is_null() {
        return Err(last_error("CryptMsgOpenToEncode"));
    }
    let message = Message(handle);
    let last = parts.len().saturating_sub(1);
    for (i, part) in parts.iter().enumerate() {
        // SAFETY: hands over `part` (alive for the call); the final part
        // makes crypt32 sign, which may show the card's PIN dialog.
        let ok = unsafe {
            CryptMsgUpdate(
                message.0,
                part.as_ptr(),
                part.len() as u32,
                (i == last) as i32,
            )
        };
        if ok == 0 {
            return Err(last_error("CryptMsgUpdate"));
        }
    }
    if parts.is_empty() {
        // SAFETY: an empty final update.
        if unsafe { CryptMsgUpdate(message.0, null(), 0, 1) } == 0 {
            return Err(last_error("CryptMsgUpdate"));
        }
    }
    let mut size = 0u32;
    // SAFETY: a size query.
    if unsafe { CryptMsgGetParam(message.0, CMSG_CONTENT_PARAM, 0, null_mut(), &mut size) } == 0 {
        return Err(last_error("CryptMsgGetParam"));
    }
    let mut out = vec![0u8; size as usize];
    // SAFETY: `out` has the `size` bytes asked for.
    let ok = unsafe {
        CryptMsgGetParam(
            message.0,
            CMSG_CONTENT_PARAM,
            0,
            out.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if ok == 0 {
        return Err(last_error("CryptMsgGetParam"));
    }
    out.truncate(size as usize);
    // `message` was declared last, so it is closed before the data its
    // parameters point to is freed.
    Ok(out)
}

/// Checks a detached signature of `parts` (the signature itself, not the
/// signer's trust).
pub(crate) fn verify(cms: &[u8], parts: &[&[u8]]) -> Result<(), SignError> {
    let para = CRYPT_VERIFY_MESSAGE_PARA {
        cbSize: size_of::<CRYPT_VERIFY_MESSAGE_PARA>() as u32,
        dwMsgAndCertEncodingType: ENCODING,
        ..CRYPT_VERIFY_MESSAGE_PARA::default()
    };
    let pointers: Vec<*const u8> = parts.iter().map(|p| p.as_ptr()).collect();
    let lengths: Vec<u32> = parts.iter().map(|p| p.len() as u32).collect();
    // SAFETY: the signature and every part outlive the call; the arrays
    // have one entry per part.
    let ok = unsafe {
        CryptVerifyDetachedMessageSignature(
            &para,
            0,
            cms.as_ptr(),
            cms.len() as u32,
            parts.len() as u32,
            pointers.as_ptr(),
            lengths.as_ptr(),
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error("CryptVerifyDetachedMessageSignature"));
    }
    Ok(())
}

pub(crate) fn sign(signer: &Signer, parts: &[&[u8]]) -> Result<Vec<u8>, SignError> {
    let cms = match signer {
        Signer::Store { thumbprint, window } => {
            let store = Store::personal().ok_or(SignError::CertificateMissing)?;
            let cert = find(&store, thumbprint).ok_or(SignError::CertificateMissing)?;
            let key = Key::of(&cert, *window)?;
            encode(&cert, key.handle, key.spec, &chain(&cert), parts)?
        }
        Signer::Ephemeral => {
            let ephemeral = Ephemeral::new()?;
            let certificates = [ephemeral.cert.encoded().to_vec()];
            encode(
                &ephemeral.cert,
                ephemeral.key.0,
                CERT_NCRYPT_KEY_SPEC,
                &certificates,
                parts,
            )?
        }
    };
    verify(&cms, parts)?;
    Ok(cms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_signatures_verify_and_cover_every_part() {
        let parts: [&[u8]; 2] = [b"%PDF-1.7 first part", b"second part %%EOF"];
        let cms = sign(&Signer::Ephemeral, &parts).expect("an ephemeral signature");
        // A ContentInfo: SEQUENCE { OID signedData ... }.
        assert_eq!(cms[0], 0x30);
        let signed_data_oid = [
            0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02,
        ];
        assert!(cms.windows(11).any(|w| w == signed_data_oid));
        // The signing-certificate-v2 attribute (its OID's last arcs).
        let ess = [
            0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x10, 0x02, 0x2F,
        ];
        assert!(cms.windows(ess.len()).any(|w| w == ess));
        // No signing-time attribute (1.2.840.113549.1.9.5).
        let signing_time = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x05];
        assert!(!cms.windows(signing_time.len()).any(|w| w == signing_time));
        // Detached: the signed bytes are not inside.
        assert!(!cms.windows(10).any(|w| w == &b"first part"[..]));
        verify(&cms, &parts).expect("verifies");
        let changed: [&[u8]; 2] = [b"%PDF-1.7 first part", b"second part %%EOF "];
        assert!(verify(&cms, &changed).is_err(), "a change breaks it");
    }

    #[test]
    fn unknown_certificates_are_missing() {
        let signer = Signer::Store {
            thumbprint: [0x5A; 20],
            window: 0,
        };
        assert_eq!(sign(&signer, &[b"x"]), Err(SignError::CertificateMissing));
        // Listing reads the store and never fails.
        let _ = certificates();
    }

    #[test]
    fn card_and_cancel_errors_are_told_apart() {
        assert_eq!(
            classify("x", SCARD_W_CANCELLED_BY_USER),
            SignError::Cancelled
        );
        assert_eq!(classify("x", ERROR_CANCELLED), SignError::Cancelled);
        assert_eq!(classify("x", SCARD_W_WRONG_CHV), SignError::WrongPin);
        assert_eq!(classify("x", SCARD_W_REMOVED_CARD), SignError::CardMissing);
        assert_eq!(classify("x", 5), SignError::Failed { what: "x", code: 5 });
    }
}
