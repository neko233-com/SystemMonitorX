//! On-demand Authenticode verification of the current file on disk.
//!
//! Run only in a user-requested blocking/background task. This verifies the
//! Windows-selected embedded signature, or local SHA256/SHA1 catalog members
//! when no embedded signature exists. It does not verify process memory, fetch
//! missing catalogs/certificates, or check online revocation. Version-resource
//! CompanyName is never used as a signer.
//!
//! API contracts:
//! https://learn.microsoft.com/windows/win32/api/wintrust/nf-wintrust-winverifytrust
//! https://learn.microsoft.com/windows/win32/api/wintrust/ns-wintrust-wintrust_data
//! https://learn.microsoft.com/windows/win32/api/wintrust/ns-wintrust-wintrust_catalog_info
//! https://learn.microsoft.com/windows/win32/api/mscat/nf-mscat-cryptcatadminenumcatalogfromhash

use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom},
    mem::size_of,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{
            CERT_E_CHAINING, CERT_E_EXPIRED, CERT_E_REVOKED, CERT_E_UNTRUSTEDROOT,
            CRYPT_E_HASH_VALUE, ERROR_NOT_FOUND, ERROR_SUCCESS, FILETIME, GetLastError, HANDLE,
            HWND, NTE_BAD_SIGNATURE, SetLastError, TRUST_E_ACTION_UNKNOWN, TRUST_E_BAD_DIGEST,
            TRUST_E_EXPLICIT_DISTRUST, TRUST_E_MALFORMED_SIGNATURE, TRUST_E_NOSIGNATURE,
            TRUST_E_PROVIDER_UNKNOWN, TRUST_E_SUBJECT_FORM_UNKNOWN, TRUST_E_SUBJECT_NOT_TRUSTED,
            TRUST_E_SYSTEM_ERROR,
        },
        Security::{
            Cryptography::{
                CERT_NAME_ATTR_TYPE,
                Catalog::{
                    CATALOG_INFO, CryptCATAdminAcquireContext2,
                    CryptCATAdminCalcHashFromFileHandle2, CryptCATAdminEnumCatalogFromHash,
                    CryptCATAdminReleaseCatalogContext, CryptCATAdminReleaseContext,
                    CryptCATCatalogInfoFromContext,
                },
                CertGetNameStringW, szOID_COMMON_NAME,
            },
            WinTrust::{
                CRYPT_PROVIDER_SGNR, SGNR_TYPE_TIMESTAMP, WINTRUST_ACTION_GENERIC_VERIFY_V2,
                WINTRUST_CATALOG_INFO, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
                WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_CATALOG, WTD_CHOICE_FILE,
                WTD_DISABLE_MD2_MD4, WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE,
                WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
                WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
                WTHelperProvDataFromStateData, WinVerifyTrust,
            },
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_SHARE_READ, GetFileInformationByHandle,
        },
    },
    core::{PCWSTR, w},
};

const MAX_CATALOGS: usize = 32;
const MAX_NAME_UNITS: usize = 16 * 1024;
const OFFLINE_NOTE: &str = "仅使用本机证书信任与缓存；未联网查询证书吊销状态";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignatureStatus {
    VerifiedOffline,
    /// No embedded signature and no member in the searched local catalog index.
    /// External/uninstalled catalogs are outside this result's scope.
    Unsigned,
    Invalid,
    Untrusted,
    #[default]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignatureKind {
    Embedded,
    Catalog,
    None,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureTimestamp {
    /// WinTrust's verification-as-of FILETIME, supplied only when the provider
    /// accepted a timestamp countersigner. This is not a certificate validity
    /// date, nor an independent assertion of the time the executable was built.
    pub verification_time_windows_ticks: u64,
    pub authority_name: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Default)]
pub struct FileSignature {
    pub status: SignatureStatus,
    /// Subject CN from the actual successfully verified signing chain only.
    pub signer_name: Option<String>,
    pub signature_kind: SignatureKind,
    pub timestamp: Option<SignatureTimestamp>,
    pub reason: String,
    pub file_path: PathBuf,
    pub file_size: u64,
    pub modified_windows_ticks: u64,
    pub volume_serial_number: u32,
    pub file_id: u64,
    pub wintrust_code: Option<i32>,
    pub catalog_path: Option<PathBuf>,
    /// Always false: cache-only verification deliberately performs no revocation
    /// check. A trusted cached chain does not establish current non-revocation.
    pub revocation_checked: bool,
    pub issues: Vec<String>,
}

/// Verifies one current disk file while denying concurrent writes/deletion.
/// Missing files, directories, and failure to acquire the read guard return Err.
/// Trust/provider failures return a FileSignature with a precise status/reason.
pub fn verify_file_signature(path: &Path) -> Result<FileSignature> {
    let mut file = locked_file(path)?;
    let mut identity = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: File owns this live read handle throughout every verification.
    unsafe { GetFileInformationByHandle(file_handle(&file), &mut identity) }
        .context("无法读取待验签文件身份")?;
    let canonical = path.canonicalize().context("无法解析待验签文件路径")?;
    let path_wide = wide_path(&canonical)?;
    let mut result = FileSignature {
        file_path: canonical,
        file_size: pair_u64(identity.nFileSizeHigh, identity.nFileSizeLow),
        modified_windows_ticks: filetime_ticks(identity.ftLastWriteTime),
        volume_serial_number: identity.dwVolumeSerialNumber,
        file_id: pair_u64(identity.nFileIndexHigh, identity.nFileIndexLow),
        revocation_checked: false,
        ..Default::default()
    };
    file.seek(SeekFrom::Start(0))
        .context("无法定位待验签文件")?;
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path_wide.as_ptr()),
        hFile: file_handle(&file),
        ..Default::default()
    };
    let state = TrustState::verify(WINTRUST_DATA {
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        ..trust_data()
    });
    let code = state.code;
    result.wintrust_code = Some(code);
    if code != TRUST_E_NOSIGNATURE.0 {
        apply_trust(&mut result, &state, SignatureKind::Embedded);
        return Ok(result);
    }
    // Close embedded provider state before opening catalog verification states.
    drop(state);
    let catalog_result = verify_catalogs(&mut file, &path_wide, &mut result);
    if let Err(error) = catalog_result {
        result.status = SignatureStatus::Unavailable;
        result.signature_kind = SignatureKind::Unknown;
        result.signer_name = None;
        result.timestamp = None;
        result.reason =
            format!("目录签名检查不完整，无法判定是否未签名：{error:#}；{OFFLINE_NOTE}");
    }
    Ok(result)
}

fn locked_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        .context("无法锁定待验签文件：文件不存在、权限不足或正在被写入")?;
    ensure!(file.metadata()?.is_file(), "待验签路径不是普通文件");
    Ok(file)
}

fn wide_path(path: &Path) -> Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    ensure!(!units.contains(&0), "文件路径含有 NUL");
    ensure!(units.len() < 32767, "文件路径超出 Windows API 范围");
    units.push(0);
    Ok(units)
}

fn file_handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn trust_data() -> WINTRUST_DATA {
    WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE | WTD_DISABLE_MD2_MD4,
        ..Default::default()
    }
}

/// Borrows file/catalog descriptors and their string storage until drop.
/// Each VERIFY call is paired with CLOSE, including unsuccessful verification.
struct TrustState {
    data: Box<WINTRUST_DATA>,
    code: i32,
}

impl TrustState {
    fn verify(data: WINTRUST_DATA) -> Self {
        let mut state = Self {
            data: Box::new(data),
            code: 0,
        };
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: caller keeps the union's descriptor and all referenced memory
        // alive until this state is dropped. -1 disallows interactive UI.
        state.code = unsafe {
            WinVerifyTrust(
                HWND(-1isize as *mut _),
                &mut action,
                (&mut *state.data as *mut WINTRUST_DATA).cast(),
            )
        };
        state
    }

    fn verified_signer(&self) -> Option<*mut CRYPT_PROVIDER_SGNR> {
        if self.code != 0 || self.data.hWVTStateData.is_invalid() {
            return None;
        }
        // SAFETY: provider state is live; only the successful trusted state is
        // consulted, never an untrusted PKCS#7 subject supplied by the file.
        unsafe {
            let provider = WTHelperProvDataFromStateData(self.data.hWVTStateData);
            if provider.is_null() || (*provider).csSigners == 0 {
                return None;
            }
            let signer = WTHelperGetProvSignerFromChain(provider, 0, false, 0);
            if signer.is_null() || (*signer).dwError != 0 {
                return None;
            }
            Some(signer)
        }
    }
}

impl Drop for TrustState {
    fn drop(&mut self) {
        self.data.dwStateAction = WTD_STATEACTION_CLOSE;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: CLOSE is required after every VERIFY. Callers declare this
        // guard after its borrowed descriptors, so they remain live here.
        unsafe {
            WinVerifyTrust(
                HWND(-1isize as *mut _),
                &mut action,
                (&mut *self.data as *mut WINTRUST_DATA).cast(),
            );
        }
    }
}

fn apply_trust(result: &mut FileSignature, state: &TrustState, kind: SignatureKind) {
    result.status = classify_status(state.code);
    result.signature_kind = kind;
    result.wintrust_code = Some(state.code);
    result.signer_name = None;
    result.timestamp = None;
    result.reason = format!("{}；{OFFLINE_NOTE}", trust_reason(state.code));
    if let Some(signer) = state.verified_signer() {
        result.signer_name = signer_common_name(signer);
        result.timestamp = trusted_timestamp(&state.data, signer);
    }
    if result.status == SignatureStatus::VerifiedOffline && result.signer_name.is_none() {
        result
            .issues
            .push("验签通过，但认证链的签名证书 CN 无法读取；不使用版本资源代替发布者".into());
    }
}

fn classify_status(code: i32) -> SignatureStatus {
    if code == 0 {
        SignatureStatus::VerifiedOffline
    } else if [
        TRUST_E_BAD_DIGEST.0,
        NTE_BAD_SIGNATURE.0,
        TRUST_E_MALFORMED_SIGNATURE.0,
        CRYPT_E_HASH_VALUE.0,
    ]
    .contains(&code)
    {
        SignatureStatus::Invalid
    } else if [
        TRUST_E_PROVIDER_UNKNOWN.0,
        TRUST_E_ACTION_UNKNOWN.0,
        TRUST_E_SUBJECT_FORM_UNKNOWN.0,
        TRUST_E_SYSTEM_ERROR.0,
        TRUST_E_NOSIGNATURE.0,
    ]
    .contains(&code)
    {
        SignatureStatus::Unavailable
    } else if code == TRUST_E_SUBJECT_NOT_TRUSTED.0
        || code == TRUST_E_EXPLICIT_DISTRUST.0
        || (0x800B0101..=0x800B0114).contains(&(code as u32))
    {
        SignatureStatus::Untrusted
    } else {
        SignatureStatus::Unavailable
    }
}

fn trust_reason(code: i32) -> String {
    let detail = match code {
        0 => "磁盘文件内容与 Authenticode 签名及本机证书信任策略验证通过",
        value if value == TRUST_E_BAD_DIGEST.0 || value == CRYPT_E_HASH_VALUE.0 => {
            "文件内容摘要与签名不匹配"
        }
        value if value == NTE_BAD_SIGNATURE.0 || value == TRUST_E_MALFORMED_SIGNATURE.0 => {
            "签名无效或签名数据损坏"
        }
        value if value == CERT_E_UNTRUSTEDROOT.0 || value == CERT_E_CHAINING.0 => {
            "签名证书链未获得本机信任或链不完整"
        }
        value if value == CERT_E_EXPIRED.0 => "签名证书不在有效期且未获得有效时间戳策略认可",
        value if value == CERT_E_REVOKED.0 => "本机信任策略报告证书已吊销",
        value if value == TRUST_E_EXPLICIT_DISTRUST.0 => "本机信任策略明确拒绝此签名",
        value if value == TRUST_E_NOSIGNATURE.0 => "没有找到可验证的签名",
        value if value == TRUST_E_SUBJECT_FORM_UNKNOWN.0 => "Windows 不支持此文件的签名格式",
        _ => "Windows Authenticode 验证未通过或验证服务不可用",
    };
    format!("{detail} (WinTrust 0x{:08X})", code as u32)
}

fn signer_common_name(signer: *mut CRYPT_PROVIDER_SGNR) -> Option<String> {
    // SAFETY: signer is supplied by a live successful provider state. Index 0
    // is the signer leaf certificate, rather than an issuer/root company name.
    unsafe {
        if signer.is_null() || (*signer).csCertChain == 0 || (*signer).dwError != 0 {
            return None;
        }
        let cert = WTHelperGetProvCertFromChain(signer, 0);
        if cert.is_null() || (*cert).pCert.is_null() || (*cert).dwError != 0 {
            return None;
        }
        let oid = Some(szOID_COMMON_NAME.as_ptr().cast());
        let required = CertGetNameStringW((*cert).pCert, CERT_NAME_ATTR_TYPE, 0, oid, None);
        if !(2..=MAX_NAME_UNITS as u32).contains(&required) {
            return None;
        }
        let mut buffer = vec![0u16; required as usize];
        let written = CertGetNameStringW(
            (*cert).pCert,
            CERT_NAME_ATTR_TYPE,
            0,
            oid,
            Some(&mut buffer),
        );
        bounded_wide_text(&buffer, written)
    }
}

fn trusted_timestamp(
    data: &WINTRUST_DATA,
    signer: *mut CRYPT_PROVIDER_SGNR,
) -> Option<SignatureTimestamp> {
    // sftVerifyAsOf alone can be current time; it is never exposed as a
    // timestamp without a successfully verified timestamp countersigner.
    // SAFETY: all pointers remain owned by the successful WinTrust state.
    unsafe {
        let provider = WTHelperProvDataFromStateData(data.hWVTStateData);
        if provider.is_null() || (*signer).csCounterSigners == 0 {
            return None;
        }
        for index in 0..(*signer).csCounterSigners.min(16) {
            let counter = WTHelperGetProvSignerFromChain(provider, 0, true, index);
            if counter.is_null()
                || (*counter).dwError != 0
                || (*counter).dwSignerType & SGNR_TYPE_TIMESTAMP == 0
                || (*counter).csCertChain == 0
            {
                continue;
            }
            let ticks = filetime_ticks((*signer).sftVerifyAsOf);
            if ticks == 0 {
                return None;
            }
            return Some(SignatureTimestamp {
                verification_time_windows_ticks: ticks,
                authority_name: signer_common_name(counter),
                source: "WinTrust 已认可的时间戳签名及 verification-as-of FILETIME".into(),
            });
        }
    }
    None
}

fn bounded_wide_text(buffer: &[u16], written: u32) -> Option<String> {
    let length = usize::try_from(written).ok()?;
    if length < 2 || length > buffer.len() || buffer[length - 1] != 0 {
        return None;
    }
    let content = &buffer[..length - 1];
    if content.contains(&0) {
        return None;
    }
    let text = String::from_utf16(content).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

struct CatalogAdmin(isize);

impl CatalogAdmin {
    fn acquire(algorithm: PCWSTR) -> Result<Self> {
        let mut handle = 0;
        // SAFETY: OS-provided catalog read context; no catalogs are added.
        unsafe { CryptCATAdminAcquireContext2(&mut handle, None, algorithm, None, Some(0)) }
            .context("无法读取本机目录签名索引")?;
        ensure!(handle != 0, "目录签名索引返回空句柄");
        Ok(Self(handle))
    }
}

impl Drop for CatalogAdmin {
    fn drop(&mut self) {
        // SAFETY: the context is uniquely owned; cursor is dropped before it.
        unsafe {
            let _ = CryptCATAdminReleaseContext(self.0, 0);
        }
    }
}

/// The API consumes the previous enumeration context when advancing, as in
/// Microsoft's codesigning.cpp sample. Early return releases the current one.
struct CatalogCursor<'a> {
    admin: &'a CatalogAdmin,
    current: isize,
}

impl CatalogCursor<'_> {
    fn next(&mut self, hash: &[u8]) -> Result<bool> {
        // SAFETY: the API receives only its own previous catalog context.
        unsafe {
            SetLastError(ERROR_SUCCESS);
            self.current = CryptCATAdminEnumCatalogFromHash(
                self.admin.0,
                hash,
                Some(0),
                Some(&mut self.current),
            );
            if self.current == 0 {
                let error = GetLastError();
                ensure!(
                    error == ERROR_SUCCESS || error == ERROR_NOT_FOUND,
                    "枚举目录签名失败：Windows {}",
                    error.0
                );
            }
        }
        Ok(self.current != 0)
    }
}

impl Drop for CatalogCursor<'_> {
    fn drop(&mut self) {
        if self.current != 0 {
            // SAFETY: current is the one remaining owned enumeration handle.
            unsafe {
                let _ = CryptCATAdminReleaseCatalogContext(self.admin.0, self.current, 0);
            }
        }
    }
}

fn catalog_hash(file: &mut File, admin: &CatalogAdmin) -> Result<Vec<u8>> {
    let mut length = 0;
    file.seek(SeekFrom::Start(0))?;
    // SAFETY: valid locked file; a null buffer asks the API for required size.
    unsafe {
        CryptCATAdminCalcHashFromFileHandle2(admin.0, file_handle(file), &mut length, None, Some(0))
    }
    .context("无法计算目录成员哈希长度")?;
    ensure!((1..=128).contains(&length), "目录成员哈希长度异常");
    let mut hash = vec![0u8; length as usize];
    file.seek(SeekFrom::Start(0))?;
    // SAFETY: the allocated capacity matches the API-requested byte count.
    unsafe {
        CryptCATAdminCalcHashFromFileHandle2(
            admin.0,
            file_handle(file),
            &mut length,
            Some(hash.as_mut_ptr()),
            Some(0),
        )
    }
    .context("无法计算目录成员哈希")?;
    ensure!(
        length > 0 && length as usize <= hash.len(),
        "目录成员哈希返回长度异常"
    );
    hash.truncate(length as usize);
    Ok(hash)
}

fn member_tag(hash: &[u8]) -> Vec<u16> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    hash.iter()
        .flat_map(|byte| {
            [
                HEX[(byte >> 4) as usize] as u16,
                HEX[(byte & 15) as usize] as u16,
            ]
        })
        .chain([0])
        .collect()
}

fn verify_catalogs(file: &mut File, path_wide: &[u16], result: &mut FileSignature) -> Result<()> {
    let mut checked = 0usize;
    let mut failed: Option<(i32, PathBuf)> = None;
    let mut incomplete = Vec::new();
    for algorithm in [w!("SHA256"), w!("SHA1")] {
        let admin = match CatalogAdmin::acquire(algorithm) {
            Ok(admin) => admin,
            Err(error) => {
                incomplete.push(format!("{error:#}"));
                continue;
            }
        };
        let mut hash = match catalog_hash(file, &admin) {
            Ok(hash) => hash,
            Err(error) => {
                incomplete.push(format!("{error:#}"));
                continue;
            }
        };
        let tag = member_tag(&hash);
        let mut cursor = CatalogCursor {
            admin: &admin,
            current: 0,
        };
        loop {
            match cursor.next(&hash) {
                Ok(false) => break,
                Ok(true) => {}
                Err(error) => {
                    incomplete.push(format!("{error:#}"));
                    break;
                }
            }
            if checked >= MAX_CATALOGS {
                incomplete.push(format!("匹配目录超过 {MAX_CATALOGS} 项，剩余目录未验证"));
                break;
            }
            checked += 1;
            let mut catalog = CATALOG_INFO {
                cbStruct: size_of::<CATALOG_INFO>() as u32,
                ..Default::default()
            };
            // SAFETY: catalog context is live until the next cursor step.
            if let Err(error) =
                unsafe { CryptCATCatalogInfoFromContext(cursor.current, &mut catalog, 0) }
            {
                incomplete.push(format!("无法读取匹配目录路径：{error}"));
                continue;
            }
            let Some(end) = catalog.wszCatalogFile.iter().position(|unit| *unit == 0) else {
                incomplete.push("匹配目录路径未在固定缓冲区终止".into());
                continue;
            };
            if end == 0 {
                incomplete.push("匹配目录路径为空".into());
                continue;
            }
            let catalog_path = PathBuf::from(OsString::from_wide(&catalog.wszCatalogFile[..end]));
            // Prevent replacement of the catalog file while the provider reads it.
            let _catalog_guard = match locked_file(&catalog_path) {
                Ok(guard) => guard,
                Err(error) => {
                    incomplete.push(format!("{error:#}"));
                    continue;
                }
            };
            file.seek(SeekFrom::Start(0))?;
            let mut info = WINTRUST_CATALOG_INFO {
                cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
                pcwszCatalogFilePath: PCWSTR(catalog.wszCatalogFile.as_ptr()),
                pcwszMemberTag: PCWSTR(tag.as_ptr()),
                pcwszMemberFilePath: PCWSTR(path_wide.as_ptr()),
                hMemberFile: file_handle(file),
                pbCalculatedFileHash: hash.as_mut_ptr(),
                cbCalculatedFileHash: hash.len() as u32,
                hCatAdmin: admin.0,
                ..Default::default()
            };
            let state = TrustState::verify(WINTRUST_DATA {
                dwUnionChoice: WTD_CHOICE_CATALOG,
                Anonymous: WINTRUST_DATA_0 {
                    pCatalog: &mut info,
                },
                ..trust_data()
            });
            if state.code == 0 {
                apply_trust(result, &state, SignatureKind::Catalog);
                result.catalog_path = Some(catalog_path);
                result.issues.extend(incomplete);
                return Ok(());
            }
            // Keep searching: a later matching catalog may have a trusted chain.
            if failed.is_none() {
                failed = Some((state.code, catalog_path));
            }
        }
    }
    result.issues.extend(incomplete);
    if let Some((code, path)) = failed {
        result.status = if result.issues.is_empty() {
            classify_status(code)
        } else {
            SignatureStatus::Unavailable
        };
        result.signature_kind = SignatureKind::Catalog;
        result.wintrust_code = Some(code);
        result.catalog_path = Some(path);
        result.reason = format!(
            "已检查 {checked} 个匹配目录，均未通过：{}；{OFFLINE_NOTE}",
            trust_reason(code)
        );
    } else if result.issues.is_empty() {
        result.status = SignatureStatus::Unsigned;
        result.signature_kind = SignatureKind::None;
        result.reason = format!(
            "未找到嵌入式签名或本机 SHA256/SHA1 目录成员；不包括未安装的外部目录；{OFFLINE_NOTE}"
        );
    } else {
        result.status = SignatureStatus::Unavailable;
        result.signature_kind = SignatureKind::Unknown;
        result.reason =
            format!("未找到已验证签名，但目录检查不完整，不能断言未签名；{OFFLINE_NOTE}");
    }
    Ok(())
}

fn pair_u64(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn filetime_ticks(time: FILETIME) -> u64 {
    pair_u64(time.dwHighDateTime, time.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use super::{
        SignatureKind, SignatureStatus, bounded_wide_text, classify_status, locked_file,
        member_tag, trust_data, verify_file_signature,
    };
    use std::{
        fs::{self, OpenOptions},
        os::windows::fs::OpenOptionsExt,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    use windows::Win32::{
        Foundation::{CERT_E_UNTRUSTEDROOT, TRUST_E_BAD_DIGEST, TRUST_E_NOSIGNATURE},
        Security::WinTrust::{
            WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE,
            WTD_STATEACTION_VERIFY, WTD_UI_NONE,
        },
        Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE},
    };

    static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "SystemMonitorX-signature-{}-{nonce}-{}",
                std::process::id(),
                FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_signature_does_not_claim_unsigned_without_catalog_search() {
        assert_eq!(
            classify_status(TRUST_E_NOSIGNATURE.0),
            SignatureStatus::Unavailable
        );
        assert_eq!(classify_status(0), SignatureStatus::VerifiedOffline);
        assert_eq!(classify_status(1), SignatureStatus::Unavailable);
        assert_eq!(
            classify_status(TRUST_E_BAD_DIGEST.0),
            SignatureStatus::Invalid
        );
        assert_eq!(
            classify_status(CERT_E_UNTRUSTEDROOT.0),
            SignatureStatus::Untrusted
        );
    }

    #[test]
    fn policy_is_noninteractive_cache_only_without_revocation() {
        let data = trust_data();
        assert_eq!(data.dwUIChoice, WTD_UI_NONE);
        assert_eq!(data.dwStateAction, WTD_STATEACTION_VERIFY);
        assert_eq!(data.fdwRevocationChecks, WTD_REVOKE_NONE);
        assert_eq!(
            data.dwProvFlags & WTD_CACHE_ONLY_URL_RETRIEVAL,
            WTD_CACHE_ONLY_URL_RETRIEVAL
        );
        assert_eq!(
            data.dwProvFlags & WTD_REVOCATION_CHECK_NONE,
            WTD_REVOCATION_CHECK_NONE
        );
    }

    #[test]
    fn certificate_text_requires_bounded_terminated_valid_utf16() {
        assert_eq!(bounded_wide_text(&[65, 0], 2).as_deref(), Some("A"));
        assert_eq!(
            bounded_wide_text(&[0xD83D, 0xDE00, 0], 3).as_deref(),
            Some("😀")
        );
        assert!(bounded_wide_text(&[65, 0], 3).is_none());
        assert!(bounded_wide_text(&[65, 66], 2).is_none());
        assert!(bounded_wide_text(&[65, 0, 66, 0], 4).is_none());
        assert!(bounded_wide_text(&[0xD800, 0], 2).is_none());
        assert!(bounded_wide_text(&[0], 1).is_none());
    }

    #[test]
    fn catalog_member_tag_is_uppercase_hash_not_a_filename() {
        assert_eq!(
            member_tag(&[0, 0x0F, 0x80, 0xFF]),
            "000F80FF\0".encode_utf16().collect::<Vec<_>>()
        );
    }

    #[test]
    fn guard_prevents_write_and_delete_until_verification_finishes() {
        let fixture = Fixture::new();
        let path = fixture.0.join("owned.exe");
        fs::write(&path, b"owned signature fixture").unwrap();
        let guard = locked_file(&path).unwrap();
        assert!(
            OpenOptions::new()
                .write(true)
                .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
                .open(&path)
                .is_err()
        );
        assert!(fs::remove_file(&path).is_err());
        drop(guard);
        assert!(OpenOptions::new().write(true).open(&path).is_ok());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn nonexistent_and_directory_are_rejected() {
        let fixture = Fixture::new();
        assert!(verify_file_signature(&fixture.0.join("missing.exe")).is_err());
        assert!(verify_file_signature(&fixture.0).is_err());
    }

    #[test]
    fn owned_unsigned_test_executable_never_supplies_a_publisher() {
        let fixture = Fixture::new();
        let path = fixture.0.join("owned-unsigned.exe");
        fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
        let result = verify_file_signature(&path).unwrap();
        assert_eq!(result.status, SignatureStatus::Unsigned, "{result:?}");
        assert_eq!(result.signature_kind, SignatureKind::None);
        assert!(result.signer_name.is_none());
        assert!(result.timestamp.is_none());
        assert!(!result.revocation_checked);
        assert_eq!(result.file_size, fs::metadata(&path).unwrap().len());
        assert!(result.modified_windows_ticks > 0);
    }
}
