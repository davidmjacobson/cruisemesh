use std::{ptr, slice};

use anyhow::{bail, Context, Result};
use windows_sys::Win32::{
    Foundation::{GetLastError, LocalFree},
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    },
};

const DESCRIPTION: &[u16] = &[
    b'C' as u16,
    b'r' as u16,
    b'u' as u16,
    b'i' as u16,
    b's' as u16,
    b'e' as u16,
    b'M' as u16,
    b'e' as u16,
    b's' as u16,
    b'h' as u16,
    0,
];

pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>> {
    if plaintext.is_empty() {
        bail!("refusing to DPAPI-protect an empty payload");
    }
    let input = blob_for(plaintext);
    let mut output = empty_blob();
    let ok = unsafe {
        CryptProtectData(
            &input,
            DESCRIPTION.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        bail!("CryptProtectData failed with Windows error {}", unsafe {
            GetLastError()
        });
    }
    take_blob(output).context("failed to copy CryptProtectData output")
}

pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>> {
    if ciphertext.is_empty() {
        bail!("DPAPI payload is empty");
    }
    let input = blob_for(ciphertext);
    let mut output = empty_blob();
    let mut description = ptr::null_mut();
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            &mut description,
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        bail!("CryptUnprotectData failed with Windows error {}", unsafe {
            GetLastError()
        });
    }
    if !description.is_null() {
        unsafe {
            LocalFree(description.cast());
        }
    }
    take_blob(output).context("failed to copy CryptUnprotectData output")
}

fn blob_for(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}

fn empty_blob() -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    }
}

fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>> {
    if blob.pbData.is_null() || blob.cbData == 0 {
        bail!("DPAPI returned an empty payload");
    }
    let bytes = unsafe { slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec() };
    unsafe {
        LocalFree(blob.pbData.cast());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_for_the_current_windows_user() {
        let plaintext = b"CruiseMesh secret";
        let protected = protect(plaintext).unwrap();
        assert_ne!(protected, plaintext);
        assert_eq!(unprotect(&protected).unwrap(), plaintext);
    }
}
