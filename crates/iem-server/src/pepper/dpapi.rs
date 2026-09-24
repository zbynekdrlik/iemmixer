//! DPAPI (current user) protection of the PIN pepper — Windows only.
//! Excluded from mutation testing (not compiled on Linux CI); the `windows`
//! CI job runs `pepper::` tests against it.

use std::io;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

pub(super) fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob_for(data)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `data` for the duration of the call; on success
    // DPAPI fills `output` with a LocalAlloc'd buffer that `take_output` frees.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(io::Error::other)?;
    Ok(take_output(output))
}

pub(super) fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob_for(data)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(io::Error::other)?;
    Ok(take_output(output))
}

fn blob_for(data: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
    let len = u32::try_from(data.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI input too large"))?;
    Ok(CRYPT_INTEGER_BLOB {
        cbData: len,
        pbData: data.as_ptr().cast_mut(),
    })
}

fn take_output(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: DPAPI set pbData/cbData to a valid LocalAlloc'd buffer.
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    // SAFETY: the buffer came from LocalAlloc and is freed exactly once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    bytes
}
